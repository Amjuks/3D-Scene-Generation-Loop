"""Reopen native and interchange scenes in Blender and record measured parity."""
import json
import math
import sys
from pathlib import Path

import bpy
from mathutils import Vector


def inspect():
    bpy.context.view_layer.update()
    graph = bpy.context.evaluated_depsgraph_get()
    lo, hi = [math.inf] * 3, [-math.inf] * 3
    count = triangles = 0
    for obj in bpy.context.scene.objects:
        if obj.type not in {'MESH', 'CURVE'}:
            continue
        evaluated = obj.evaluated_get(graph)
        mesh = evaluated.to_mesh()
        triangles += sum(len(p.vertices) - 2 for p in mesh.polygons)
        if mesh.vertices:
            count += 1
            for vertex in mesh.vertices:
                p = evaluated.matrix_world @ vertex.co
                for axis in range(3):
                    lo[axis] = min(lo[axis], p[axis])
                    hi[axis] = max(hi[axis], p[axis])
        evaluated.to_mesh_clear()
    return {"meshes": count, "triangles": triangles,
            "bounds": {"min": lo, "max": hi},
            "materials": len(bpy.data.materials),
            "images": [{"name": im.name, "size": list(im.size),
                        "packed": im.packed_file is not None}
                       for im in bpy.data.images if im.type == 'IMAGE']}


folder = Path(sys.argv[sys.argv.index('--') + 1]).resolve()
bpy.ops.wm.open_mainfile(filepath=str(folder / 'scene.blend'))
native = inspect()
scene = bpy.context.scene
camera_name = scene.camera.name
areas = [(o.name,list(o.location),list(o.rotation_euler),o.data.energy,
          o.data.size,list(o.data.color)) for o in scene.objects
         if o.type == 'LIGHT' and o.data.type == 'AREA']
background = scene.world.node_tree.nodes['Background']
world_color = list(background.inputs['Color'].default_value)
world_strength = background.inputs['Strength'].default_value
bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=str(folder / 'scene.glb'))
interchange = inspect()
error = max(abs(native['bounds'][side][axis] - interchange['bounds'][side][axis])
            for side in ('min', 'max') for axis in range(3))
passed = native['meshes'] > 0 and interchange['meshes'] > 0 and error < .005
report = {"geometry_parity_passed": passed,
          "maximum_bounds_difference_m": error,
          "native": native, "glb": interchange,
          "note": "Geometry parity does not establish visual or material equivalence."}
scene = bpy.context.scene
scene.world = bpy.data.worlds.new('Restored inspection environment')
scene.world.use_nodes = True
background = scene.world.node_tree.nodes['Background']
background.inputs['Color'].default_value = world_color
background.inputs['Strength'].default_value = world_strength
for name, loc, rot, energy, size, color in areas:
    data = bpy.data.lights.new(name, 'AREA')
    data.energy, data.size, data.color = energy, size, color
    data.shape = 'DISK'
    obj = bpy.data.objects.new(name, data)
    scene.collection.objects.link(obj)
    obj.location, obj.rotation_euler = loc, rot
    obj.visible_glossy = False
    obj.visible_camera = False
    obj.visible_transmission = False
scene.camera = bpy.data.objects.get(camera_name)
if scene.camera is None:
    scene.camera = next(o for o in scene.objects if o.type == 'CAMERA')
scene.render.engine = 'CYCLES'
scene.cycles.samples = 32
scene.cycles.use_denoising = True
scene.render.threads_mode = 'FIXED'
scene.render.threads = 24
scene.render.resolution_x, scene.render.resolution_y = 1000, 750
scene.render.resolution_percentage = 100
scene.view_settings.view_transform = 'AgX'
scene.view_settings.look = 'AgX - Medium High Contrast'
scene.view_settings.exposure = .3
scene.render.filepath = str(folder / 'glb-check.png')
bpy.ops.render.render(write_still=True)
report['glb_inspection_render'] = 'glb-check.png'
report['inspection_lighting'] = 'Native area lights and world restored; GLB punctual lights retained.'
(folder / 'export-validation.json').write_text(json.dumps(report, indent=2))
print(json.dumps({"folder": str(folder), "parity": passed, "bounds_difference_m": error}))
if not passed:
    raise RuntimeError('Export geometry validation failed')
