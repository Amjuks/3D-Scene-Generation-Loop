"""Authored illumination must change atmosphere without changing source geometry."""
import copy
from pathlib import Path
import sys

import bpy
from mathutils import Vector
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'crates/loop-scene/backends/blender'))
import presentation
import scene_backend as worker

worker.reset()
bpy.ops.mesh.primitive_cube_add()
mesh=bpy.context.object.data
night={'purpose':'warm exhibition spotlights after dark','sky':'constant','background_color':[.01,.02,.04],
       'strength':.1,'exposure':1,'sun':None,'lights':[{'id':'joinery','purpose':'show carved wood',
       'kind':'spot','position':[0,-3,4],'target':[0,0,1],'color':[1,.85,.65],'power_w':700,'size_m':.2,'spot_angle_degrees':50}]}
report=presentation.apply(bpy.context.scene,night)
assert report['mode']=='scene_authored'
lights=[o for o in bpy.context.scene.objects if o.type=='LIGHT']
assert len(lights)==1 and lights[0].data.type=='SPOT'
direction=lights[0].rotation_euler.to_matrix()@Vector((0,0,-1))
assert direction.dot((Vector((0,0,1))-lights[0].location).normalized())>.99999
assert bpy.context.scene.view_settings.exposure==1
day=copy.deepcopy(night)
day.update(purpose='clear afternoon outdoors',sky='physical_sky',exposure=0,lights=[],sun={
    'elevation_degrees':30,'azimuth_degrees':90,'energy':2,'color':[1,.96,.9],'angular_size_degrees':.53})
presentation.apply(bpy.context.scene,day)
lights=[o for o in bpy.context.scene.objects if o.type=='LIGHT']
assert len(lights)==1 and lights[0].data.type=='SUN'
sky=next(n for n in bpy.context.scene.world.node_tree.nodes if n.type=='TEX_SKY')
assert sky.sky_type=='NISHITA' and not sky.sun_disc
assert bpy.data.objects['Cube'].data==mesh and len(mesh.vertices)==8
before=bpy.context.scene.world
bad=copy.deepcopy(night)
bad['lights'][0]['power_w']=float('nan')
try: presentation.apply(bpy.context.scene,bad)
except ValueError: pass
else: raise AssertionError('nonfinite light accepted')
assert bpy.context.scene.world==before
worker.configure_render(bpy.context.scene,{'renderer':'cycles','resolution':[3840,2160],'samples':512})
assert bpy.context.scene.render.resolution_x==3840 and bpy.context.scene.cycles.samples==512
assert bpy.context.scene.world==before
print('AUTHORED LIGHTS, SKY, GEOMETRY PRESERVATION AND FINAL SETTINGS PASSED',flush=True)
