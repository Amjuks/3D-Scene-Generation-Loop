"""Real Blender source QA regressions; tiny fixtures are intentionally synthetic.

blender --background --factory-startup --disable-autoexec --python-exit-code 31 \
  --python tests/blender_asset_quality_check.py
"""
import hashlib
import json
from pathlib import Path
import sys
import tempfile

import bpy

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'crates/loop-scene/backends/blender'))
import asset_quality as qa


def codes(report, level='errors'):
    return {entry['code'] for entry in report[level]}


def mesh_object(vertices, faces):
    mesh = bpy.data.meshes.new('fixture-mesh')
    mesh.from_pydata(vertices, [], faces)
    obj = bpy.data.objects.new('fixture', mesh)
    bpy.context.scene.collection.objects.link(obj)
    bpy.context.view_layer.update()
    return obj


qa.reset()
obj = mesh_object([(0,0,0), (1,0,0), (1,1,0), (0,1,0)], [(0,1,2,3)])
report = qa.inspect_loaded([obj])
assert report['technical_status'] == 'valid_with_warnings', report
assert report['summary']['triangles'] == 2
assert report['summary']['bounds']['dimensions_m'] == [1,1,0]
assert 'open_or_nonmanifold_surface' in codes(report, 'warnings')
assert not report['errors'], 'Open foliage-card topology must not be rejected'
material = bpy.data.materials.new('fixture-material')
material.use_nodes = True
obj.data.materials.append(material)
image = bpy.data.images.new('fixture-texture', width=16, height=8)
image.pixels[0] = .2  # Populate a pixel buffer before packing a generated image.
image.pack()
node = material.node_tree.nodes.new('ShaderNodeTexImage')
node.image = image
report = qa.inspect_loaded([obj], max_triangles=1, max_texture_bytes=1)
assert 'textured_mesh_without_uv' in codes(report, 'warnings')
assert {'consumer_triangle_budget', 'consumer_texture_budget'} <= codes(report, 'warnings')
assert report['textures'][0]['width'] == 16 and report['textures'][0]['packed']
obj.data.uv_layers.new(name='UVMap')
report = qa.inspect_loaded([obj])
assert 'textured_mesh_without_uv' not in codes(report, 'warnings')
obj.data.uv_layers[0].data[0].uv[0] = float('nan')
assert 'nonfinite_uv' in codes(qa.inspect_loaded([obj]))
obj.data.uv_layers[0].data[0].uv[0] = 0
image = bpy.data.images.load('/missing/texture.png', check_existing=False) if Path('/missing/texture.png').exists() else bpy.data.images.new('missing-image', width=1, height=1)
image.source = 'FILE'
image.filepath = '/missing/texture.png'
node.image = image
report = qa.inspect_loaded([obj])
assert 'missing_texture_dependency' in codes(report), report
qa.reset()
obj = mesh_object([(0,0,0), (1,0,0), (2,0,0)], [(0,1,2)])
assert 'degenerate_triangles' in codes(qa.inspect_loaded([obj]))
obj.data.vertices[0].co.x = float('nan')
assert 'nonfinite_geometry' in codes(qa.inspect_loaded([obj]))
qa.reset()
bpy.ops.mesh.primitive_cube_add(size=2)
obj = bpy.context.object
obj.scale.z = 0
bpy.context.view_layer.update()
assert 'singular_transform' in codes(qa.inspect_loaded([obj]))
obj.scale.z = 1
obj.location.x = float('nan')
bpy.context.view_layer.update()
assert 'nonfinite_transform' in codes(qa.inspect_loaded([obj]))
with tempfile.TemporaryDirectory() as directory:
    directory = Path(directory)
    qa.reset()
    bpy.ops.mesh.primitive_cube_add(size=2)
    bpy.context.object.name = 'exact-object'
    source = directory / 'fixture.blend'
    bpy.ops.wm.save_as_mainfile(filepath=str(source))
    sha = hashlib.sha256(source.read_bytes()).hexdigest()
    request = {'path': str(source), 'source': {'id': 'test', 'sha256': sha, 'object_name': 'exact-object',
               'license': 'CC0-1.0', 'creator': 'test', 'source_url': 'local:test', 'dimensions_m': [2,2,2]}}
    report = qa.inspect_source(request)
    assert not report['errors'], report
    assert report['summary']['triangles'] == 12
    assert report['source']['measured_sha256'] == sha
    assert 'declared_dimensions_mismatch' not in codes(report, 'warnings')
    request['source']['sha256'] = '0'*64
    report = qa.inspect_source(request)
    assert report['technical_status'] == 'invalid' and 'SHA-256' in report['errors'][0]['message']
    request['source']['sha256'] = sha
    request['source']['object_name'] = 'not-present'
    assert qa.inspect_source(request)['technical_status'] == 'invalid'
    request['source']['object_name'] = 'exact-object'
    request['source']['dimensions_m'] = [1,1,1]
    request['allowed_licenses'] = ['user-owned']
    report = qa.inspect_source(request)
    assert 'license_policy' in codes(report)
    assert 'declared_dimensions_mismatch' in codes(report, 'warnings')
    qa.atomic_json(directory / 'report.json', report)
    assert json.loads((directory / 'report.json').read_text()) == report
print('ALL SOURCE ASSET QUALITY CHECKS PASSED', flush=True)
