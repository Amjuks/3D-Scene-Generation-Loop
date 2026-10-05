"""Run with Blender --background --python; checks evaluated recipe envelopes."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "crates/loop-scene/backends/blender"))
import scene_backend as worker
import architecture
import voxel
import interiors

for recipe in sorted(architecture.RECIPES | voxel.RECIPES | interiors.RECIPES | {"cabinet", "door", "plant", "light", "shelf"}):
    worker.reset()
    node = {"node_id": recipe, "kind": recipe, "revision": 1,
            "bounds_local": {"min": [-6, -3, 0], "max": [6, 3, 4]},
            "generation": {"recipe": recipe, "strategy": "recipe", "seed": 1},
            "materials": [{"material_id": "test", "base_color": [.2,.3,.4,1]}]}
    root = worker.node_root(node, apply_transform=False)
    worker.geometry(node, root)
    report = worker.inspect_scene()
    assert report["mesh_count"] > 0, recipe
    assert recipe in report["semantic_node_ids"], recipe
    if recipe in voxel.RECIPES:
        assert report["triangles"] > 120, (recipe, report)
    for axis in range(3):
        assert report["bounds_canonical"]["min"][axis] >= node["bounds_local"]["min"][axis] - .001, (recipe,report)
        assert report["bounds_canonical"]["max"][axis] <= node["bounds_local"]["max"][axis] + .001, (recipe,report)
    print("RECIPE PASSED:", recipe, report["mesh_count"], "meshes", flush=True)
print("ALL ARCHITECTURE ENVELOPES PASSED", flush=True)

# Axial circulation must agree with actual shell openings, not only a prompt.
import bpy
from mathutils import Vector
for circulation in ['cross','spine_x','spine_y','perimeter']:
    worker.reset()
    node={'node_id':'route-test','kind':'room_shell','revision':1,
          'bounds_local':{'min':[-4,-4,0],'max':[4,4,3.6]},
          'generation':{'recipe':'room_shell','strategy':'recipe','parameters':{'circulation':circulation}},
          'materials':[{'material_id':'route-test','base_color':[.4,.4,.4,1]}]}
    worker.geometry(node,worker.node_root(node,apply_transform=False))
    bpy.context.view_layer.update()
    for axis in [0,1]:
        direction=Vector((1,0,0) if axis==0 else (0,1,0))
        hit=bpy.context.scene.ray_cast(bpy.context.evaluated_depsgraph_get(),Vector((0,0,1)),direction,distance=5)[0]
        expected=(circulation=='spine_x' and axis==1) or (circulation=='spine_y' and axis==0)
        assert hit==expected,(circulation,axis,hit)
print('ALL CIRCULATION DOORWAYS PASSED',flush=True)

for rise in [-2.0, 0.0, 2.0]:
    worker.reset()
    node={'node_id':'bridge-slope','kind':'bridge','revision':1,
          'bounds_local':{'min':[-6,-1.5,0],'max':[6,1.5,abs(rise)+2.2]},
          'generation':{'recipe':'bridge','strategy':'recipe',
                        'parameters':{'rise':rise,'start_height':max(0,-rise)}},
          'materials':[{'material_id':'bridge-test','base_color':[.4,.4,.4,1]}]}
    worker.geometry(node,worker.node_root(node,apply_transform=False))
    report=worker.inspect_scene()
    for axis in range(3):
        assert report['bounds_canonical']['min'][axis]>=node['bounds_local']['min'][axis]-.001,(rise,report)
        assert report['bounds_canonical']['max'][axis]<=node['bounds_local']['max'][axis]+.001,(rise,report)
    deck=bpy.data.objects['bridge-slope.deck']
    endpoints={-1:[],1:[]}
    for vertex in deck.data.vertices:
        p=deck.matrix_world @ vertex.co
        endpoints[-1 if p.x<0 else 1].append(p.z)
    assert abs(min(endpoints[1])-min(endpoints[-1])-rise)<1e-5,(rise,endpoints)
print('ALL MODERN BRIDGE SLOPES PASSED',flush=True)

# collection-60 used the table recipe for a 20mm pigment mixing palette.
# Absolute minimum top/leg thickness previously escaped its allocated volume.
for size in [(0.2,0.3,0.02),(2.0,1.0,0.8)]:
    worker.reset()
    node={'node_id':'palette','kind':'table','revision':1,
          'bounds_local':{'min':[-size[0]/2,-size[1]/2,0],'max':[size[0]/2,size[1]/2,size[2]]},
          'generation':{'recipe':'table','strategy':'recipe','seed':1},
          'materials':[{'material_id':'palette','base_color':[.4,.3,.2,1]}]}
    worker.geometry(node,worker.node_root(node,apply_transform=False))
    report=worker.inspect_scene()
    for axis in range(3):
        assert report['bounds_canonical']['min'][axis]>=node['bounds_local']['min'][axis]-.001,(size,report)
        assert report['bounds_canonical']['max'][axis]<=node['bounds_local']['max'][axis]+.001,(size,report)
print('TABLE AND THIN PALETTE ENVELOPES PASSED',flush=True)
