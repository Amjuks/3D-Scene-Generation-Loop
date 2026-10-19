"""Opt-in, measured source mesh derivatives. Never overwrite catalogue originals.

The acceptance metric is deterministic bidirectional *sampled* surface distance,
not a guarantee of silhouette, shading, topology or screen-space equivalence.
Every derivative still needs visual review at its intended viewing distance.
"""
import fcntl
import hashlib
import json
import math
import os
import re
import shutil
import tempfile
from pathlib import Path

import bpy
from mathutils import Vector
from mathutils.bvhtree import BVHTree

ALGORITHM = "measured-collapse-v1"
MAX_CACHE_BYTES = 2 * 1024 ** 3
MAX_ENTRY_BYTES = 512 * 1024 ** 2
MAX_SAMPLES = 25000


def settings(value):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) - {
            'ratio', 'max_surface_error_ratio', 'max_bounds_error_ratio'}:
        raise ValueError('asset_lod requires ratio and optional measured error tolerances')
    result = {'ratio': value.get('ratio'),
              'max_surface_error_ratio': value.get('max_surface_error_ratio', .005),
              'max_bounds_error_ratio': value.get('max_bounds_error_ratio', .005)}
    for name, number in result.items():
        if isinstance(number, bool) or not isinstance(number, (float, int)) or not math.isfinite(number):
            raise ValueError(f'asset_lod {name} must be a finite number')
    if not .1 <= result['ratio'] < 1:
        raise ValueError('asset_lod ratio must be >= 0.1 and < 1.0')
    if not all(0 <= result[name] <= .02 for name in result if name != 'ratio'):
        raise ValueError('asset_lod error tolerances must be within 0..0.02 of source diagonal')
    return result


def sha256(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def cache_root():
    # Only the host process configures this; no planner-supplied cache paths.
    base = os.environ.get('LOOP_SCENE_LOD_CACHE')
    return Path(base).expanduser() if base else Path.home() / '.cache/loop-scene/asset-lod'


def identity(source_hash, selection, policy):
    data = {'algorithm': ALGORITHM, 'source_sha256': source_hash,
            'source_object': selection, 'settings': policy,
            'blender_version': list(bpy.app.version)}
    key = hashlib.sha256(json.dumps(data, sort_keys=True).encode()).hexdigest()
    return key, data


def cached(source_hash, selection, policy):
    key, expected = identity(source_hash, selection, policy)
    directory = cache_root() / key
    if not directory.exists():
        return None
    manifest = json.loads((directory / 'manifest.json').read_text())
    blend = directory / 'derivative.blend'
    if manifest.get('identity') != expected or manifest.get('blend_sha256') != sha256(blend):
        raise ValueError('LOD derivative cache failed integrity verification; remove the damaged cache entry')
    with bpy.data.libraries.load(str(blend), link=False) as (source, target):
        target.objects = source.objects
    objects = [obj for obj in target.objects if obj is not None]
    for obj in objects:
        bpy.context.scene.collection.objects.link(obj)
    report = dict(manifest['report'], cache_hit=True, artifact_path=str(blend),
                  artifact_sha256=manifest['blend_sha256'])
    for obj in objects:
        if obj.type == 'MESH':
            obj['loop_asset_lod'] = json.dumps(report, sort_keys=True)
    directory.touch()
    return objects


def _surface(mesh, transform):
    mesh.calc_loop_triangles()
    vertices = [transform @ vertex.co for vertex in mesh.vertices]
    faces = [tuple(face.vertices) for face in mesh.loop_triangles]
    if not vertices or not faces or not all(math.isfinite(v) for point in vertices for v in point):
        raise ValueError('LOD source or derivative has empty/nonfinite geometry')
    return vertices, faces


def _samples(vertices, faces):
    # Deterministic stratification includes extremes plus surface interiors;
    # report sample counts, so capped measurement cannot be mistaken for proof.
    vertex_count, face_count = min(len(vertices), MAX_SAMPLES), min(len(faces), MAX_SAMPLES)
    points = [vertices[i * len(vertices) // vertex_count] for i in range(vertex_count)]
    for axis in range(3):
        points.extend((min(vertices, key=lambda p: p[axis]), max(vertices, key=lambda p: p[axis])))
    points.extend(sum((vertices[j] for j in faces[i * len(faces) // face_count]), Vector()) / 3
                  for i in range(face_count))
    return points


def _measure(before, after):
    old_vertices, old_faces = before
    new_vertices, new_faces = after
    old_tree = BVHTree.FromPolygons(old_vertices, old_faces, all_triangles=True)
    new_tree = BVHTree.FromPolygons(new_vertices, new_faces, all_triangles=True)
    old_samples, new_samples = _samples(*before), _samples(*after)
    distance = max(tree.find_nearest(point)[3]
                   for tree, samples in ((new_tree, old_samples), (old_tree, new_samples))
                   for point in samples)
    old_bounds = [[op(point[axis] for point in old_vertices) for axis in range(3)] for op in (min, max)]
    new_bounds = [[op(point[axis] for point in new_vertices) for axis in range(3)] for op in (min, max)]
    diagonal = (Vector(old_bounds[1]) - Vector(old_bounds[0])).length
    if diagonal <= 1e-12:
        raise ValueError('LOD source has degenerate bounds')
    bounds_error = max(abs(old_bounds[side][axis] - new_bounds[side][axis])
                       for side in range(2) for axis in range(3))
    return {'source_triangles': len(old_faces), 'derivative_triangles': len(new_faces),
            'sampled_surface_error_m': distance, 'sampled_surface_error_ratio': distance / diagonal,
            'bounds_error_m': bounds_error, 'bounds_error_ratio': bounds_error / diagonal,
            'source_bounds': old_bounds, 'derivative_bounds': new_bounds,
            'source_samples': len(old_samples), 'derivative_samples': len(new_samples)}


def derive(objects, source_hash, selection, policy):
    """Copy/bake each unique evaluated mesh, preserving source file and shaders."""
    bpy.context.view_layer.update()
    deps = bpy.context.evaluated_depsgraph_get()
    corners = [obj.evaluated_get(deps).matrix_world @ Vector(corner)
               for obj in objects if obj.type == 'MESH'
               for corner in obj.evaluated_get(deps).bound_box]
    source_bounds = {'min': [min(p[axis] for p in corners) for axis in range(3)],
                     'max': [max(p[axis] for p in corners) for axis in range(3)]}
    reports, derived = [], {}
    for obj in objects:
        if obj.type != 'MESH':
            continue
        # Modifiers can be object-specific. Share only when no such evaluation
        # differences exist, and include transform scale for meter-based metrics.
        key = (obj.data.as_pointer(), tuple(v for row in obj.matrix_world.to_3x3() for v in row)) if not obj.modifiers else None
        if key is not None and key in derived:
            obj.data = derived[key]
            continue
        evaluated = obj.evaluated_get(deps)
        original = bpy.data.meshes.new_from_object(evaluated, preserve_all_data_layers=True, depsgraph=deps)
        before = _surface(original, obj.matrix_world)
        uv_names = [layer.name for layer in original.uv_layers]
        material_slots = list(original.materials)
        # Evaluate on a disposable object, so source mesh, modifiers and hierarchy
        # are not mutated if quality rejection raises an error.
        probe = bpy.data.objects.new('loop_lod_probe', original)
        bpy.context.scene.collection.objects.link(probe)
        probe.matrix_world = obj.matrix_world.copy()
        modifier = probe.modifiers.new('Measured source LOD', 'DECIMATE')
        modifier.decimate_type = 'COLLAPSE'
        modifier.ratio = policy['ratio']
        modifier.use_collapse_triangulate = True
        bpy.context.view_layer.update()
        reduced = None
        try:
            reduced = bpy.data.meshes.new_from_object(probe.evaluated_get(deps), preserve_all_data_layers=True, depsgraph=deps)
            report = dict(_measure(before, _surface(reduced, obj.matrix_world)), source_object=obj.name)
            if report['sampled_surface_error_ratio'] > policy['max_surface_error_ratio'] + 1e-8:
                raise ValueError(f"LOD {obj.name} sampled surface error {report['sampled_surface_error_ratio']:.6g} exceeds requested tolerance")
            if report['bounds_error_ratio'] > policy['max_bounds_error_ratio'] + 1e-8:
                raise ValueError(f"LOD {obj.name} bounds error {report['bounds_error_ratio']:.6g} exceeds requested tolerance")
            if [layer.name for layer in reduced.uv_layers] != uv_names or list(reduced.materials) != material_slots:
                raise ValueError('LOD lost UV layers or material slots')
            if any(not math.isfinite(c) for layer in reduced.uv_layers for item in layer.data for c in item.uv):
                raise ValueError('LOD contains nonfinite UVs')
            obj.data = reduced
            obj.modifiers.clear()
            if key is not None:
                derived[key] = reduced
            reports.append(report)
        except Exception:
            if reduced is not None:
                bpy.data.meshes.remove(reduced)
            raise
        finally:
            bpy.data.objects.remove(probe, do_unlink=True)
            bpy.data.meshes.remove(original)
    report = {'algorithm': ALGORITHM, 'source_sha256': source_hash, 'source_object': selection,
              'source_bounds': source_bounds,
              'settings': policy, 'measurement': 'bidirectional stratified vertex/triangle-center sampling',
              'visual_review_required': True, 'unique_meshes': reports,
              'source_triangles': sum(r['source_triangles'] for r in reports),
              'derivative_triangles': sum(r['derivative_triangles'] for r in reports),
              'cache_hit': False}
    if report['derivative_triangles'] >= report['source_triangles']:
        raise ValueError('LOD request produced no triangle reduction')
    _store(objects, source_hash, selection, policy, report)
    for obj in objects:
        if obj.type == 'MESH':
            obj['loop_asset_lod'] = json.dumps(report, sort_keys=True)
    return report


def _store(objects, source_hash, selection, policy, report):
    key, source_identity = identity(source_hash, selection, policy)
    root = cache_root()
    root.mkdir(parents=True, exist_ok=True)
    destination = root / key
    with tempfile.TemporaryDirectory(prefix='.build-', dir=root) as tmp:
        tmp = Path(tmp)
        blend = tmp / 'derivative.blend'
        bpy.data.libraries.write(str(blend), set(objects), fake_user=True, compress=True)
        if blend.stat().st_size > MAX_ENTRY_BYTES:
            raise ValueError('LOD derivative exceeds the 512 MiB per-entry cache limit')
        checksum = sha256(blend)
        (tmp / 'manifest.json').write_text(json.dumps({'identity': source_identity, 'blend_sha256': checksum,
                                                     'report': report}, indent=2, sort_keys=True) + '\n')
        with (root / '.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if not destination.exists():
                entries = [p for p in root.iterdir() if p.is_dir() and not p.is_symlink()
                           and re.fullmatch(r'[0-9a-f]{64}', p.name)]
                sizes = {p: sum(f.stat().st_size for f in p.iterdir() if f.is_file()) for p in entries}
                required = sum(f.stat().st_size for f in tmp.iterdir())
                total = sum(sizes.values()) + required
                for entry in sorted(entries, key=lambda p: p.stat().st_mtime):
                    if total <= MAX_CACHE_BYTES:
                        break
                    shutil.rmtree(entry)
                    total -= sizes[entry]
                # Atomic publication means readers never see a partial artifact.
                os.rename(tmp, destination)
        # Another worker may have won publication; retain its verified digest.
        manifest = json.loads((destination / 'manifest.json').read_text())
        if manifest['identity'] != source_identity:
            raise ValueError('LOD cache identity collision')
        report.update(artifact_path=str(destination / 'derivative.blend'),
                      artifact_sha256=manifest['blend_sha256'])
