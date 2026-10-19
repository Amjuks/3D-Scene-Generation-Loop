#!/usr/bin/env python3
"""Audit generated scene structure without treating requested repetition as a failure.

Reads saved scene-spec.json artifacts, not concept names or material colors. Exact
fingerprints are invariant to node renaming, list order, translation and uniform
scale. Approximate matches are review candidates, never automatic rejections.
These are structural proxies; they do not replace image or mesh-level review.
"""
import argparse
from collections import Counter, defaultdict
import hashlib
import itertools
import json
import math
from pathlib import Path


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()).hexdigest()


def transform_point(point, transform):
    scale = transform.get('scale', [1, 1, 1])
    x, y, z = [point[i] * scale[i] for i in range(3)]
    qx, qy, qz, qw = transform.get('rotation_xyzw', [0, 0, 0, 1])
    # v' = v + 2(qw(q_xyz x v) + q_xyz x (q_xyz x v)).
    uv = [qy*z-qz*y, qz*x-qx*z, qx*y-qy*x]
    uuv = [qy*uv[2]-qz*uv[1], qz*uv[0]-qx*uv[2], qx*uv[1]-qy*uv[0]]
    offset = transform.get('translation', [0, 0, 0])
    return [v + 2*(qw*uv[i]+uuv[i])+offset[i] for i, v in enumerate([x, y, z])]


def describe(spec):
    nodes = {node['node_id']: node for node in spec['nodes']}
    if len(nodes) != len(spec['nodes']):
        raise ValueError('duplicate node IDs')
    leaves = []
    recipes, strategies = Counter(), Counter()
    for node in nodes.values():
        generation = node['generation']
        recipes[generation['recipe']] += 1
        strategies[generation['strategy']] += 1
        if generation['strategy'] == 'assembly' or generation['recipe'] in {'light', 'light_beam'}:
            continue
        bounds = node['bounds_local']
        corners = list(itertools.product(*zip(bounds['min'], bounds['max'])))
        parent, seen, ancestry = node, set(), []
        while parent:
            if parent['node_id'] in seen:
                raise ValueError('cyclic scene hierarchy')
            seen.add(parent['node_id'])
            corners = [transform_point(corner, parent.get('transform', {})) for corner in corners]
            ancestry.append(parent['generation']['recipe'])
            parent_id = parent.get('parent_id')
            if parent_id is not None and parent_id not in nodes:
                raise ValueError(f'missing parent {parent_id}')
            parent = nodes.get(parent_id)
        low = [min(c[i] for c in corners) for i in range(3)]
        high = [max(c[i] for c in corners) for i in range(3)]
        if not all(math.isfinite(v) for v in low + high) or any(high[i] <= low[i] for i in range(3)):
            raise ValueError('invalid or empty geometry bounds')
        parameters = generation.get('parameters', {})
        # Content identity is distinct from user labels/paths. Include authored
        # geometry so two different meshes with equal bounds are not exact copies.
        detail = {key: value for key, value in parameters.items() if key in {
            'asset_sha256', 'vertices', 'faces', 'profile', 'path', 'points',
            'heights', 'holes', 'radius', 'segments', 'cross_section'}}
        leaves.append((generation['recipe'], low, high, ancestry, digest(detail)))
    if not leaves:
        raise ValueError('scene has no non-light geometry leaves')
    origin = [min(leaf[1][i] for leaf in leaves) for i in range(3)]
    extent = [max(leaf[2][i] for leaf in leaves)-origin[i] for i in range(3)]
    unit = max(extent)
    geometry, occupancy = [], Counter()
    for recipe, low, high, ancestry, detail in leaves:
        normalized = [round((v-origin[i % 3])/unit, 5) for i, v in enumerate(low+high)]
        geometry.append([recipe, normalized, ancestry, detail])
        cell = [min(3, max(0, int(4*((low[i]+high[i])/2-origin[i])/max(extent[i], 1e-9)))) for i in range(3)]
        occupancy[f'{recipe}:{cell}'] += 1
    geometry.sort(key=lambda item: json.dumps(item, sort_keys=True))
    return {
        'geometry_fingerprint': digest(geometry),
        'nodes': len(nodes), 'geometry_leaves': len(leaves),
        'extent_m': extent, 'recipes': dict(sorted(recipes.items())),
        'strategies': dict(sorted(strategies.items())),
        'spatial_histogram': dict(sorted(occupancy.items())),
    }


def histogram_similarity(a, b):
    keys = a.keys() | b.keys()
    return sum(min(a.get(k, 0), b.get(k, 0)) for k in keys) / max(1, sum(max(a.get(k, 0), b.get(k, 0)) for k in keys))


def audit(paths, nearest=5):
    scenes, errors = [], []
    for path in paths:
        try:
            scenes.append({'path': str(path), **describe(json.loads(path.read_text()))})
        except (ValueError, OSError, KeyError, TypeError) as error:
            errors.append({'path': str(path), 'error': str(error)})
    groups, recipes = defaultdict(list), Counter()
    neighbors = [[] for _ in scenes]
    for index, scene in enumerate(scenes):
        groups[scene['geometry_fingerprint']].append(scene['path'])
        recipes.update(scene['recipes'])
        for other_index in range(index):
            other = scenes[other_index]
            recipe = histogram_similarity(scene['recipes'], other['recipes'])
            spatial = histogram_similarity(scene['spatial_histogram'], other['spatial_histogram'])
            score = round((recipe + spatial)/2, 4)
            for own, peer in ((index, other_index), (other_index, index)):
                neighbors[own].append({'path': scenes[peer]['path'], 'score': score,
                    'recipe_similarity': round(recipe, 4), 'spatial_similarity': round(spatial, 4),
                    'same_geometry_fingerprint': scene['geometry_fingerprint'] == other['geometry_fingerprint']})
                neighbors[own] = sorted(neighbors[own], key=lambda item: (-item['score'], item['path']))[:nearest]
    for index, scene in enumerate(scenes):
        scene['nearest_scenes'] = neighbors[index]
        del scene['spatial_histogram']
    return {'version': 1,
        'method': 'Saved generated geometry bounds, hierarchy, recipes, authored geometry and asset content identities; excludes labels, prompts, palette, seed and absolute translation/scale. Approximate scores are review aids, not quality or duplicate verdicts. Mesh surface appearance requires visual review.',
        'scene_count': len(scenes), 'recipe_totals': dict(recipes.most_common()),
        'repeated_geometry_groups': [group for group in groups.values() if len(group) > 1],
        'scenes': scenes, 'errors': errors}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', nargs='+', type=Path, help='Batch directories, run directories or scene-spec.json files')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--nearest', type=int, default=5)
    parser.add_argument('--max-scenes', type=int, default=1000, help='Bound pairwise comparisons; refuses truncation')
    args = parser.parse_args(argv)
    if args.nearest < 1 or args.max_scenes < 1:
        parser.error('nearest and max-scenes must be positive')
    paths = set()
    for item in args.inputs:
        if item.is_file():
            paths.add(item.resolve())
        elif (item / 'scenes').is_dir():
            paths.update(path.resolve() for path in (item / 'scenes').glob('*/designs/scene-spec.json'))
        elif (item / 'designs/scene-spec.json').is_file():
            paths.add((item / 'designs/scene-spec.json').resolve())
        else:
            parser.error(f'no scene specification at {item}')
    if not paths or len(paths) > args.max_scenes:
        parser.error(f'found {len(paths)} scenes; expected 1..{args.max_scenes}; increase --max-scenes explicitly if needed')
    report = audit(sorted(paths), args.nearest)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(f"Audited {report['scene_count']} scenes; {len(report['repeated_geometry_groups'])} repeated geometry groups; {len(report['errors'])} errors. Report: {args.output}")
    return 1 if report['errors'] else 0


if __name__ == '__main__':
    raise SystemExit(main())
