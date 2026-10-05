"""Render a supplementary close view from the completed native scene."""
import bpy
import sys
from pathlib import Path
from mathutils import Vector

args = sys.argv[sys.argv.index('--')+1:]
name, folder = args[0], Path(args[1]).resolve()
bpy.ops.wm.open_mainfile(filepath=str(folder/'scene.blend'))
views = {
    'japandi_reading': ((1.6,-1.9,1.30),(-.3,.35,.65),34),
    'oak_dining': ((1.8,-2.1,1.55),(-.1,.05,.84),38),
    'walnut_studio': ((1.65,1.6,1.45),(-.1,-.03,.98),39),
}
pos,target,lens = views[name]
s=bpy.context.scene
s.camera.location=pos
s.camera.rotation_euler=(Vector(target)-s.camera.location).to_track_quat('-Z','Y').to_euler()
s.camera.data.lens=lens
s.render.resolution_x,s.render.resolution_y=1600,1200
s.cycles.samples=96
s.cycles.use_denoising=True
s.render.filepath=str(folder/'detail.png')
bpy.ops.render.render(write_still=True)
