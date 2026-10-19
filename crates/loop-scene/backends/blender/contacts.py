"""Measure declared support contact against evaluated meshes in a shared frame."""
import bpy
from itertools import chain
from mathutils import Matrix, Vector
from mathutils.bvhtree import BVHTree


def mesh_data(root, frame_inverse, include_faces=True):
    vertices, faces = [], []
    deps = bpy.context.evaluated_depsgraph_get()
    for obj in root.children_recursive:
        if obj.type != 'MESH':
            continue
        evaluated = obj.evaluated_get(deps)
        mesh = evaluated.to_mesh()
        try:
            transform = frame_inverse @ evaluated.matrix_world
            offset = len(vertices)
            vertices.extend(transform @ v.co for v in mesh.vertices)
            if include_faces:
                faces.extend(tuple(offset + i for i in f.vertices) for f in mesh.polygons)
        finally:
            evaluated.to_mesh_clear()
    return vertices, faces


def contact_offset(points, surface, ceiling):
    """Exact sampled-vertex answer with a conservative support-height bound.

    A vertex above ``ceiling - best_delta`` cannot increase the correction,
    even if it hits the highest triangle. Foliage/interior vertices therefore
    need no raycast once a base vertex establishes contact. Geometry is intact.
    """
    if not points:
        raise ValueError('contact object has no evaluated vertices')
    first = min(range(len(points)), key=lambda i: points[i].z)
    best, rays, hits, pruned = None, 0, 0, 0
    margin = max(1e-7, abs(ceiling) * 1e-7)
    for index in chain((first,), range(first), range(first + 1, len(points))):
        point = points[index]
        # Float BVH arithmetic can put a hit infinitesimally above the measured
        # ceiling. A scale-relative margin only costs a few extra rays; it never
        # discards a candidate based on an optimistic rounding assumption.
        if best is not None and ceiling + margin - point.z <= best:
            pruned += 1
            continue
        rays += 1
        hit, _, _, _ = surface.ray_cast(Vector((point.x, point.y, ceiling + 1)), Vector((0, 0, -1)))
        if hit is not None:
            offset = hit.z - point.z
            best = offset if best is None else max(best, offset)
            hits += 1
    return best, {'raycast_vertices': rays, 'surface_hits': hits,
                  'height_bound_pruned_vertices': pruned}


def resolve(nodes, roots):
    by_id = {n['node_id']: n for n in nodes}
    pending = {n['node_id']: n['generation'].get('parameters', {}).get('support_node_id')
               for n in nodes if n['generation'].get('parameters', {}).get('support_node_id')}
    evidence, surfaces = [], {}
    while pending:
        progress = False
        for node_id, support_id in list(pending.items()):
            if support_id in pending:
                continue
            node, support = by_id[node_id], by_id.get(support_id)
            if support is None or node.get('parent_id') != support.get('parent_id'):
                raise ValueError('contact support must exist in the same regional frame')
            root, support_root = roots[node_id], roots[support_id]
            bpy.context.view_layer.update()
            frame_inverse = root.parent.matrix_world.inverted() if root.parent else Matrix.Identity(4)
            if support_id not in surfaces:
                points, faces = mesh_data(support_root, frame_inverse)
                if not points or not faces:
                    raise ValueError('support has no evaluated surface')
                ceiling = max(p.z for p in points)
                surfaces[support_id] = (BVHTree.FromPolygons(points, faces), ceiling)
            surface, ceiling = surfaces[support_id]
            points, _ = mesh_data(root, frame_inverse, include_faces=False)
            delta, statistics = contact_offset(points, surface, ceiling)
            if delta is None:
                raise ValueError(f'{node_id} has no mesh overlap with declared support {support_id}')
            # Smallest vertical translation avoiding sampled-vertex penetration.
            root.location.z += delta
            bpy.context.view_layer.update()
            evidence.append({'node_id': node_id, 'support_node_id': support_id,
                             'translation': list(root.location), 'delta_z_m': delta,
                             'sampled_vertices': len(points), **statistics})
            del pending[node_id]
            progress = True
        if not progress:
            raise ValueError('cyclic support references')
    return evidence


def terrain_camera_surfaces(nodes, roots):
    surfaces = []
    for node in nodes:
        if node['generation']['recipe'] != 'heightfield':
            continue
        if not any(word in node.get('kind', '').lower() for word in ('terrain', 'ground', 'landscape')):
            continue
        vertices, faces = mesh_data(roots[node['node_id']], Matrix.Identity(4))
        if vertices and faces:
            surfaces.append((BVHTree.FromPolygons(vertices, faces), max(v.z for v in vertices) + 1))
    return surfaces


def camera_clearance(position, target, surfaces, allow_subsurface=False):
    """Keep accidental underground cameras from presenting mesh undersides."""
    position, target = Vector(position), Vector(target)
    if allow_subsurface:
        return position, target, 0.0
    ground = []
    for surface, top in surfaces:
        hit, _, _, _ = surface.ray_cast(Vector((position.x, position.y, top)), Vector((0, 0, -1)))
        if hit is not None:
            ground.append(hit.z)
    delta = max(0.0, max(ground) + .1 - position.z) if ground else 0.0
    position.z += delta
    target.z += delta
    return position, target, delta
