"""Actual Blender derivative QA, source integrity, persistent cache and instancing.

Run with --background --factory-startup --disable-autoexec --python-exit-code 31.
The sphere fixture verifies ingestion/measurement, not artistic production quality.
"""
import copy
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path

import bpy

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'crates/loop-scene/backends/blender'))
import scene_backend as worker
import asset_lod


def generate(node, reset=True):
    if reset:
        worker.reset()
    objects = worker.geometry(node, worker.node_root(node))
    evidence = worker.inspect_scene()['geometry_evidence']
    return objects, next(report for report in evidence if report['node_id'] == node['node_id'])


with tempfile.TemporaryDirectory(prefix='loop-lod-test-') as temporary:
    temporary = Path(temporary)
    os.environ['LOOP_SCENE_LOD_CACHE'] = str(temporary / 'cache')
    worker.reset()
    bpy.ops.mesh.primitive_uv_sphere_add(segments=64, ring_count=32)
    source = bpy.context.object
    source.name = 'DetailedSource'
    source.rotation_euler.z = .2
    material = worker.mat({'material_id': 'textured_alpha_source', 'base_color': [.2,.7,.1,1]})
    source.data.materials.append(material)
    image = bpy.data.images.new('test alpha image', width=8, height=8, alpha=True)
    image.generated_color = (.2, .7, .1, .5)
    image.pixels[0] = .2
    image.pack()
    texture = material.node_tree.nodes.new('ShaderNodeTexImage')
    texture.image = image
    shader = material.node_tree.nodes.get('Principled BSDF')
    material.node_tree.links.new(texture.outputs['Color'], shader.inputs['Base Color'])
    material.node_tree.links.new(texture.outputs['Alpha'], shader.inputs['Alpha'])
    original_mesh = source.data
    original_mesh.calc_loop_triangles()
    original_triangles = len(original_mesh.loop_triangles)
    source_path = temporary / 'source.blend'
    bpy.data.libraries.write(str(source_path), {source}, fake_user=True)
    source_hash = hashlib.sha256(source_path.read_bytes()).hexdigest()
    node = {'node_id': 'source', 'kind': 'fixture', 'revision': 1,
            'transform': {'translation': [0,0,0], 'rotation_xyzw': [0,0,0,1]},
            'bounds_local': {'min': [-1,-1,0], 'max': [1,1,2]},
            'generation': {'recipe': 'asset', 'strategy': 'recipe', 'parameters': {
                'asset_path': str(source_path), 'asset_sha256': source_hash, 'asset_id': 'fixture'}}}
    original, default_evidence = generate(node)
    assert 'source_lod' not in default_evidence, 'full quality default unexpectedly reduced mesh'
    assert worker.inspect_scene()['triangles'] == original_triangles
    lod_node = copy.deepcopy(node)
    lod_node['generation']['parameters']['asset_lod'] = {'ratio': .5}
    reduced, evidence = generate(lod_node)
    report = evidence['source_lod']
    assert report['source_triangles'] == original_triangles
    assert report['derivative_triangles'] < original_triangles * .6
    assert report['source_sha256'] == source_hash and not report['cache_hit']
    assert evidence['uniform_scale'] == default_evidence['uniform_scale']
    assert evidence['source_bounds'] == default_evidence['source_bounds']
    assert reduced[0].data.uv_layers
    imported_shader = reduced[0].data.materials[0].node_tree.nodes.get('Principled BSDF')
    assert imported_shader.inputs['Alpha'].is_linked
    assert next(n.image for n in reduced[0].data.materials[0].node_tree.nodes if n.type == 'TEX_IMAGE').packed_file
    assert hashlib.sha256(source_path.read_bytes()).hexdigest() == source_hash
    assert Path(report['artifact_path']).is_file()
    assert asset_lod.sha256(Path(report['artifact_path'])) == report['artifact_sha256']
    derivative_triangles = worker.inspect_scene()['triangles']
    second = copy.deepcopy(lod_node)
    second['node_id'] = 'second'
    second['transform']['translation'] = [3,0,0]
    linked, second_evidence = generate(second, reset=False)
    assert reduced[0].data is linked[0].data
    assert second_evidence['linked_instance']
    assert worker.inspect_scene()['triangles'] == derivative_triangles * 2
    restored, restored_evidence = generate(lod_node)
    assert restored_evidence['source_lod']['cache_hit']
    assert worker.inspect_scene()['triangles'] == derivative_triangles
    assert restored_evidence['source_lod']['artifact_sha256'] == report['artifact_sha256']
    assert restored[0].data.materials[0].node_tree.nodes.get('Principled BSDF').inputs['Alpha'].is_linked
    print('MEASURED OPTIONAL LOD:', json.dumps(report, sort_keys=True), flush=True)

    strict = copy.deepcopy(lod_node)
    strict['generation']['parameters']['asset_lod'] = {'ratio': .1, 'max_surface_error_ratio': 0}
    try:
        generate(strict)
    except ValueError as error:
        assert 'sampled surface error' in str(error), error
    else:
        raise AssertionError('unachievable zero-error derivative accepted')
    manifest_path = Path(report['artifact_path']).with_name('manifest.json')
    manifest = json.loads(manifest_path.read_text())
    manifest['blend_sha256'] = '0' * 64
    manifest_path.write_text(json.dumps(manifest))
    try:
        generate(lod_node)
    except ValueError as error:
        assert 'integrity verification' in str(error), error
    else:
        raise AssertionError('corrupt cached derivative accepted')
    assert hashlib.sha256(source_path.read_bytes()).hexdigest() == source_hash
    print('FULL SOURCE DEFAULT, UV/ALPHA RETENTION, REJECTION AND CACHE INTEGRITY PASSED', flush=True)
