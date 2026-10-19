"""Actual mesh contact, shared regional frames, and idempotent placement."""
import sys
import random
from pathlib import Path
import bpy
from mathutils import Vector
from mathutils import Matrix
from mathutils.bvhtree import BVHTree
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'crates/loop-scene/backends/blender'))
import scene_backend as worker
import contacts

worker.reset()
region = bpy.data.objects.new('region', None)
bpy.context.scene.collection.objects.link(region)
region.location = (50, 20, 3)
region.rotation_euler.z = 0.8
roots = {}
for name in ['surface', 'rock']:
    root = bpy.data.objects.new(name, None)
    bpy.context.scene.collection.objects.link(root)
    root.parent = region
    roots[name] = root
worker.box('support mesh', (8, 8, 1), (0, 0, .5), None, roots['surface'], bevel=0)
# Irregular tetrahedron rotated away from its bounding-box bottom. Start high.
mesh = bpy.data.meshes.new('irregular')
mesh.from_pydata([(-1,0,0),(1,0,0),(0,1,0),(0,0,2)], [], [(0,2,1),(0,1,3),(1,2,3),(2,0,3)])
rock = bpy.data.objects.new('rock mesh', mesh)
bpy.context.scene.collection.objects.link(rock)
rock.parent = roots['rock']
roots['rock'].rotation_euler.y = .7
roots['rock'].location.z = 4
nodes = [{'node_id': name, 'parent_id': 'region', 'generation': {'parameters':
         {'support_node_id': 'surface'} if name == 'rock' else {}}} for name in roots]
evidence = contacts.resolve(nodes, roots)
assert evidence[0]['delta_z_m'] < -1, evidence
points, _ = contacts.mesh_data(roots['rock'], region.matrix_world.inverted())
assert abs(min(p.z for p in points) - 1) < 1e-5, points
assert abs(contacts.resolve(nodes, roots)[0]['delta_z_m']) < 1e-5
roots['rock'].location.x = 100
try:
    contacts.resolve(nodes, roots)
except ValueError as error:
    assert 'no mesh overlap' in str(error)
else:
    raise AssertionError('unsupported placement passed')
print('EVALUATED MESH CONTACT AND IDEMPOTENT PLACEMENT PASSED', flush=True)
vertices, faces = contacts.mesh_data(roots['surface'], Matrix.Identity(4))
surfaces = [(BVHTree.FromPolygons(vertices, faces), 10)]
camera, target, delta = contacts.camera_clearance((50,20,3.5), (50,20,5), surfaces)
assert abs(camera.z-4.1) < 1e-5 and abs(delta-.6) < 1e-5
assert abs(target.z-5.6) < 1e-5
camera, _, delta = contacts.camera_clearance((50,20,3.5), (50,20,5), surfaces, True)
assert camera.z == 3.5 and delta == 0
print('TERRAIN CAMERA CLEARANCE AND SUBSURFACE OPT-OUT PASSED', flush=True)

# The pruning rule must agree with exhaustive rays on irregular supports,
# including a lowest vertex that misses the surface completely.
random.seed(11)
vertices = [Vector((x, y, random.uniform(-.5, .5))) for y in range(6) for x in range(6)]
faces = []
for y in range(5):
    for x in range(5):
        index = y * 6 + x
        faces.extend(((index, index+1, index+6), (index+1, index+7, index+6)))
surface = BVHTree.FromPolygons(vertices, faces, all_triangles=True)
ceiling = max(v.z for v in vertices)
points = [Vector((-10, -10, -5))] + [Vector((random.uniform(-1, 6), random.uniform(-1, 6),
                                          random.uniform(0, 20))) for _ in range(10000)]
expected = []
for point in points:
    hit, _, _, _ = surface.ray_cast(Vector((point.x, point.y, ceiling+1)), Vector((0, 0, -1)))
    if hit is not None:
        expected.append(hit.z-point.z)
actual, statistics = contacts.contact_offset(points, surface, ceiling)
assert abs(actual-max(expected)) < 1e-8, (actual, max(expected))
assert statistics['height_bound_pruned_vertices'] > 9000, statistics
assert statistics['raycast_vertices'] + statistics['height_bound_pruned_vertices'] == len(points)
print('EXACT CONTACT PRUNING PASSED:', statistics, flush=True)
