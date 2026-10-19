"""Blender regression: hero view must look past an occluding perimeter wall."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "crates/loop-scene/backends/blender"))
import scene_backend as worker

worker.reset()
nodes = []
for name, recipe, lo, hi in [
    ("house", "voxel_house", [-4,-4,0], [4,4,8]),
    ("occluder", "wall", [-30,-12,0], [30,-10,30]),
]:
    node = {"node_id":name,"kind":recipe,"revision":1,
            "bounds_local":{"min":lo,"max":hi},
            "transform":{"translation":[0,0,0],"rotation_xyzw":[0,0,0,1]},
            "generation":{"recipe":recipe,"seed":1,"parameters":{"biome":"jungle"}},
            "materials":[{"material_id":name,"base_color":[.3,.2,.1,1]}]}
    worker.geometry(node, worker.node_root(node, apply_transform=False))
    nodes.append(node)
plans = worker.camera_plan(worker.inspect_scene(), nodes, Path("."))
assert plans[0][1].y > 0, plans[0]
assert any(p[0] == "structure_detail" for p in plans)
print("CAMERA OCCLUSION REGRESSION PASSED", flush=True)

# Authored regional views use the actual hierarchy transform and take priority
# over room heuristics. Invalid paths cannot escape the preview output folder.
import bpy
import math
from mathutils import Vector
root=bpy.data.objects['house']
root.location=(10,20,3)
root.rotation_mode='XYZ';root.rotation_euler.z=math.pi/2
bpy.context.view_layer.update()
nodes[0]['generation']['parameters']['review_cameras']=[{
    'id':'../hero','position':[0,-6,2],'target':[0,0,1],'lens_mm':55,'coverage':'hero'}]
plans=worker.camera_plan(worker.inspect_scene(),nodes,Path('.'))
assert len(plans)==4,plans
name,pos,target=plans[-1]
assert '/' not in name and '.' not in name,name
assert (pos-Vector((16,20,5))).length<1e-5,(name,pos,target)
assert (target-Vector((10,20,4))).length<1e-5,(name,pos,target)
nodes[0]['generation']['parameters']['review_cameras'][0]['position']=[float('nan'),0,0]
try:worker.camera_plan(worker.inspect_scene(),nodes,Path('.'))
except ValueError as error:assert 'finite' in str(error)
else:raise AssertionError('non-finite authored camera accepted')
print('AUTHORED CAMERA TRANSFORM AND VALIDATION PASSED',flush=True)
