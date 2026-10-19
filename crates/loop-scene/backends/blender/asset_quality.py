"""Measured source-asset inspection. Run in an isolated --disable-autoexec Blender.

Technical validity is deliberately separate from artistic/semantic suitability.
Open surfaces and foliage cards are not rejected merely for being non-manifold.
This module reads source data; it never repairs, packs or rewrites the source.
"""
import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import sys
import tempfile

import bpy

VERSION = 1
LIMITATION = ("Technical inspection does not certify artistic quality, botanical/anatomical "
              "accuracy, cultural suitability, legal ownership or suitability at a chosen "
              "camera distance. Source provenance and license are curator declarations.")


def digest(path):
    hasher = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            hasher.update(block)
    return hasher.hexdigest()


def atomic_json(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile('w', dir=path.parent, delete=False) as file:
        temporary = Path(file.name)
        json.dump(data, file, indent=2, sort_keys=True, allow_nan=False)
        file.write('\n')
    temporary.replace(path)


def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)


def _load(path, selection):
    """Append source data without opening a source scene or sanitizing evidence."""
    units = []
    if path.suffix.lower() == '.glb':
        bpy.ops.import_scene.gltf(filepath=str(path))
        units.append({'format': 'glTF', 'meters_per_unit': 1.0})
    elif path.suffix.lower() == '.blend':
        with bpy.data.libraries.load(str(path), link=False) as (source, target):
            target.objects = source.objects
            target.scenes = source.scenes
        for obj in target.objects:
            if obj is not None:
                bpy.context.scene.collection.objects.link(obj)
        for scene in target.scenes:
            if scene is not None:
                units.append({'scene': scene.name, 'system': scene.unit_settings.system,
                              'meters_per_unit': scene.unit_settings.scale_length})
    else:
        raise ValueError('Only .glb and .blend source assets are supported')
    objects = list(bpy.context.scene.objects)
    if selection:
        found = [obj for obj in objects if obj.name == selection]
        if len(found) != 1:
            raise ValueError(f'Source object {selection!r} is not present uniquely')
        keep = {found[0], *found[0].children_recursive}
        objects = [obj for obj in objects if obj in keep]
    bpy.context.view_layer.update()
    return objects, units


def _material_images(material):
    images, missing_nodes, visited = set(), [], set()

    def walk(tree, prefix):
        if tree is None or tree in visited:
            return
        visited.add(tree)
        for node in tree.nodes:
            if node.type in {'TEX_IMAGE', 'TEX_ENVIRONMENT'}:
                if node.image:
                    images.add(node.image)
                else:
                    missing_nodes.append(prefix + node.name)
            if node.type == 'GROUP':
                walk(node.node_tree, prefix + node.name + '/')
    if material and material.use_nodes:
        walk(material.node_tree, '')
    return images, missing_nodes


def _shader_evidence(material):
    """Describe supplied shader detail; it is not proof of visible relief."""
    if not material.use_nodes:
        return {'surface_connected': False, 'displacement_connected': False, 'channels': {}}
    tree = material.node_tree
    outputs = [node for node in tree.nodes if node.type == 'OUTPUT_MATERIAL' and node.is_active_output]

    def linked_images(socket):
        seen, images, kinds = set(), set(), set()
        def walk(input_socket):
            for link in input_socket.links:
                node = link.from_node
                if node in seen:
                    continue
                seen.add(node)
                kinds.add(node.type)
                if node.type in {'TEX_IMAGE', 'TEX_ENVIRONMENT'} and node.image:
                    images.add(node.image.name)
                if node.type == 'GROUP':
                    # Group inputs can have multiple internal paths. Do not
                    # pretend every internal image feeds this output channel.
                    kinds.add('GROUP_REQUIRES_MANUAL_CHANNEL_TRACE')
                for source_socket in node.inputs:
                    walk(source_socket)
        walk(socket)
        return {'linked': bool(socket.is_linked), 'images': sorted(images), 'node_types': sorted(kinds)}

    channels = {}
    for node in tree.nodes:
        if node.type == 'BSDF_PRINCIPLED':
            channels[node.name] = {name: linked_images(node.inputs[name]) for name in
                ('Base Color', 'Metallic', 'Roughness', 'Normal', 'Alpha', 'Transmission Weight') if name in node.inputs}
    displacement = [linked_images(output.inputs['Displacement']) for output in outputs]
    return {'surface_connected': any(output.inputs['Surface'].is_linked for output in outputs),
            'displacement_connected': any(output.inputs['Displacement'].is_linked for output in outputs),
            'displacement_method': getattr(material, 'displacement_method', None),
            'displacement': displacement, 'channels': channels,
            'interpretation': 'Connected normal/bump maps change shading. Connected displacement still requires renderer support and sufficient geometry. No channel completeness implies real-world accuracy.'}


def inspect_loaded(objects, *, source=None, source_units=(), max_triangles=None,
                   max_texture_bytes=None):
    """Inspect the selected original/evaluated meshes without altering them."""
    report = {'schema_version': VERSION, 'inspector': 'loop-blender-asset-quality-v1',
              'blender_version': bpy.app.version_string, 'source': source or {},
              'coordinate_basis': 'right-handed Z-up; pipeline treats one Blender unit as one meter',
              'source_units': list(source_units), 'errors': [], 'warnings': [],
              'objects': [], 'materials': [], 'textures': [], 'limitations': LIMITATION}

    def issue(level, code, message, **evidence):
        report[level].append(dict(code=code, message=message, **evidence))

    if any(abs(unit.get('meters_per_unit', 1) - 1) > 1e-6 for unit in source_units):
        issue('warnings', 'source_units_require_review', 'Blend unit settings differ from the pipeline meter convention. Confirm source dimensions before uniform fitting.')
    if not objects:
        issue('errors', 'empty_source', 'Selected source contains no objects.')
    dependencies = bpy.context.evaluated_depsgraph_get()
    materials, images, unique_meshes = set(), set(), set()
    total_vertices = total_triangles = total_degenerate = unique_triangles = 0
    boundary_edges = nonmanifold_edges = 0
    bounds_low, bounds_high = [math.inf] * 3, [-math.inf] * 3
    for obj in sorted(objects, key=lambda value: value.name):
        data = {'name': obj.name, 'type': obj.type, 'hidden_render': bool(obj.hide_render),
                'source_mesh': obj.data.name if obj.type == 'MESH' else None,
                'modifiers': [{'type': modifier.type, 'render_enabled': modifier.show_render,
                               'viewport_enabled': modifier.show_viewport} for modifier in obj.modifiers]}
        report['objects'].append(data)
        matrix = obj.matrix_world
        if not all(math.isfinite(value) for row in matrix for value in row):
            issue('errors', 'nonfinite_transform', 'Object transform contains NaN or infinity.', object=obj.name)
            continue
        if abs(matrix.to_3x3().determinant()) < 1e-15:
            issue('errors' if obj.type == 'MESH' else 'warnings', 'singular_transform', 'Object transform collapses one or more axes.', object=obj.name)
        if obj.animation_data or obj.constraints:
            issue('warnings', 'dynamic_dependencies', 'Animation or constraints need deliberate static-pose review; the scene importer clears object animation.', object=obj.name)
        if obj.type != 'MESH':
            if obj.type not in {'EMPTY', 'LIGHT', 'CAMERA'}:
                issue('warnings', 'unsupported_object_type', 'Scene import only treats mesh objects as visible source geometry.', object=obj.name, object_type=obj.type)
            continue
        if obj.hide_render:
            issue('warnings', 'hidden_render_geometry', 'Source mesh is hidden from renders.', object=obj.name)
        evaluated = obj.evaluated_get(dependencies)
        mesh = evaluated.to_mesh(preserve_all_data_layers=True, depsgraph=dependencies)
        if mesh is None:
            issue('errors', 'missing_evaluated_mesh', 'Source mesh could not be evaluated.', object=obj.name)
            continue
        try:
            mesh.calc_loop_triangles()
            points = [evaluated.matrix_world @ vertex.co for vertex in mesh.vertices]
            finite = all(math.isfinite(value) for point in points for value in point)
            triangle_count = len(mesh.loop_triangles)
            data.update(vertices=len(points), polygons=len(mesh.polygons), triangles=triangle_count,
                        uv_layers=[layer.name for layer in mesh.uv_layers],
                        material_slots=[material.name if material else None for material in mesh.materials])
            total_vertices += len(points)
            total_triangles += triangle_count
            if obj.data not in unique_meshes:
                unique_meshes.add(obj.data)
                unique_triangles += triangle_count
            if not finite:
                issue('errors', 'nonfinite_geometry', 'Evaluated mesh vertices contain NaN or infinity.', object=obj.name)
                continue
            if not points or not triangle_count:
                issue('errors', 'empty_mesh', 'Source mesh contains no renderable triangles.', object=obj.name)
                continue
            for point in points:
                for axis in range(3):
                    bounds_low[axis] = min(bounds_low[axis], point[axis])
                    bounds_high[axis] = max(bounds_high[axis], point[axis])
            degenerate = sum((points[t.vertices[1]] - points[t.vertices[0]]).cross(points[t.vertices[2]] - points[t.vertices[0]]).length_squared <= 1e-24
                             for t in mesh.loop_triangles)
            data['degenerate_triangles'] = degenerate
            total_degenerate += degenerate
            if degenerate:
                issue('errors' if degenerate == triangle_count else 'warnings', 'degenerate_triangles',
                      'Zero-area triangles cannot represent visible surface detail.', object=obj.name,
                      count=degenerate, triangles=triangle_count)
            incidence = Counter(loop.edge_index for polygon in mesh.polygons for loop in (mesh.loops[i] for i in polygon.loop_indices))
            boundary = sum(incidence[edge.index] == 1 for edge in mesh.edges)
            nonmanifold = sum(incidence[edge.index] != 2 for edge in mesh.edges)
            data.update(boundary_edges=boundary, nonmanifold_edges=nonmanifold)
            boundary_edges += boundary
            nonmanifold_edges += nonmanifold
            used_slots = set(polygon.material_index for polygon in mesh.polygons)
            bad_slots = sorted(index for index in used_slots if index >= len(mesh.materials) or mesh.materials[index] is None)
            if bad_slots:
                issue('warnings', 'unassigned_material', 'Some polygons have no assigned source material.', object=obj.name, slots=bad_slots)
            object_images = set()
            for material in mesh.materials:
                if material:
                    materials.add(material)
                    texture_images, _ = _material_images(material)
                    object_images.update(texture_images)
            if object_images and not mesh.uv_layers:
                issue('warnings', 'textured_mesh_without_uv', 'Textured mesh has no UV layer. Verify deliberate generated/object mapping before scene use.', object=obj.name)
            for layer in mesh.uv_layers:
                if any(not math.isfinite(v) for uv in layer.data for v in uv.uv):
                    issue('errors', 'nonfinite_uv', 'Mesh UV coordinates contain NaN or infinity.', object=obj.name, layer=layer.name)
        finally:
            evaluated.to_mesh_clear()
    if not total_triangles:
        issue('errors', 'no_mesh_geometry', 'Selected source contains no renderable mesh geometry.')
    if nonmanifold_edges:
        issue('warnings', 'open_or_nonmanifold_surface', 'Open/non-manifold edges may be intentional foliage, cloth or scanned surfaces; review use before solid fabrication or collision.', count=nonmanifold_edges, boundary_edges=boundary_edges)
    for material in sorted(materials, key=lambda value: value.name):
        texture_images, missing_nodes = _material_images(material)
        images.update(texture_images)
        report['materials'].append({'name': material.name, 'uses_nodes': bool(material.use_nodes),
                                    'images': sorted(image.name for image in texture_images),
                                    'empty_image_nodes': missing_nodes,
                                    'shader_evidence': _shader_evidence(material)})
        if missing_nodes:
            issue('warnings', 'empty_image_nodes', 'Material contains image nodes without an image; verify whether they affect the active shader.', material=material.name, nodes=missing_nodes)
    decoded_bytes = packed_bytes = external_bytes = 0
    for image in sorted(images, key=lambda value: value.name):
        width, height = image.size[:]
        packed = list(image.packed_files)
        packed_size = sum(item.packed_file.size for item in packed) if packed else (image.packed_file.size if image.packed_file else 0)
        is_packed = bool(packed or image.packed_file)
        image_path = Path(bpy.path.abspath(image.filepath, library=image.library)) if image.filepath else None
        external_size = image_path.stat().st_size if image_path and image_path.is_file() and not is_packed else 0
        estimate = width * height * max(image.channels, 1) * (4 if image.is_float else 1)
        data = {'name': image.name, 'source': image.source, 'width': width, 'height': height,
                'channels': image.channels, 'float_pixels': bool(image.is_float),
                'color_space': image.colorspace_settings.name, 'packed': is_packed,
                'packed_bytes': packed_size, 'external_bytes': external_size,
                'decoded_bytes_estimate': estimate,
                'external_path': str(image_path) if image_path and not is_packed else None}
        report['textures'].append(data)
        decoded_bytes += estimate
        packed_bytes += packed_size
        external_bytes += external_size
        if width <= 0 or height <= 0:
            issue('errors', 'unreadable_texture', 'Referenced texture has no decodable pixels.', image=image.name)
        if image.source == 'FILE' and not is_packed:
            if not image_path or not image_path.is_file():
                issue('errors', 'missing_texture_dependency', 'Referenced external texture does not exist.', image=image.name)
            else:
                issue('warnings', 'external_texture_dependency', 'Source is not self-contained: texture must be packed before portable distribution.', image=image.name, path=str(image_path))
        elif image.source not in {'FILE', 'GENERATED'}:
            issue('warnings', 'time_or_tile_texture_dependency', 'Sequence/movie/UDIM texture requires explicit portability and static-frame review.', image=image.name, source=image.source)
    measured_bounds = None
    if all(math.isfinite(value) for value in bounds_low + bounds_high):
        measured_bounds = {'min': bounds_low, 'max': bounds_high,
                           'dimensions_m': [hi - lo for hi, lo in zip(bounds_high, bounds_low)]}
    report['summary'] = {'objects': len(objects), 'mesh_objects': sum(obj.type == 'MESH' for obj in objects),
                         'unique_source_meshes': len(unique_meshes), 'vertices': total_vertices,
                         'triangles': total_triangles, 'unique_source_triangles': unique_triangles,
                         'degenerate_triangles': total_degenerate, 'materials': len(materials),
                         'textures': len(images), 'texture_packed_bytes': packed_bytes,
                         'texture_external_bytes': external_bytes,
                         'texture_decoded_bytes_estimate': decoded_bytes,
                         'bounds': measured_bounds}
    declared_dimensions = report['source'].get('dimensions_m')
    if declared_dimensions and measured_bounds:
        if len(declared_dimensions) != 3 or any(not isinstance(value, (float, int)) or not math.isfinite(value) or value <= 0 for value in declared_dimensions):
            issue('errors', 'invalid_declared_dimensions', 'Declared source dimensions must be three positive finite meter values.')
        elif any(abs(actual - declared) > max(0.001, declared * 0.02) for actual, declared in zip(measured_bounds['dimensions_m'], declared_dimensions)):
            issue('warnings', 'declared_dimensions_mismatch', 'Measured source dimensions differ from the catalogue by more than 2% or 1 mm.', declared=declared_dimensions, measured=measured_bounds['dimensions_m'])
    if max_triangles is not None and total_triangles > max_triangles:
        issue('warnings', 'consumer_triangle_budget', 'Source exceeds the requested per-instance triangle budget; choose appropriate LOD or another source.', triangles=total_triangles, limit=max_triangles)
    if max_texture_bytes is not None and decoded_bytes > max_texture_bytes:
        issue('warnings', 'consumer_texture_budget', 'Estimated decoded textures exceed requested consumer memory budget.', bytes=decoded_bytes, limit=max_texture_bytes)
    report['technical_status'] = 'invalid' if report['errors'] else 'valid_with_warnings' if report['warnings'] else 'valid'
    report['requires_suitability_review'] = True
    return report


def inspect_source(request):
    source = dict(request.get('source', {}))
    path = Path(request['path']).resolve()
    source['path'] = str(path)
    expected = source.get('sha256')
    try:
        size = path.stat().st_size
        if size > request.get('max_source_bytes', 2_000_000_000):
            raise ValueError('Source exceeds inspection byte limit')
        actual = digest(path)
        source.update(measured_sha256=actual, bytes=size)
        if expected and expected.casefold() != actual:
            raise ValueError('Source SHA-256 differs from catalogue identity')
        reset()
        objects, units = _load(path, source.get('object_name'))
        report = inspect_loaded(objects, source=source, source_units=units,
                                max_triangles=request.get('max_triangles'),
                                max_texture_bytes=request.get('max_texture_bytes'))
        for name in ('license', 'creator', 'source_url'):
            if not isinstance(source.get(name), str) or not source[name].strip():
                report['warnings'].append({'code': 'missing_provenance', 'message': f'Curator metadata lacks {name}.'})
        allowed = request.get('allowed_licenses')
        if allowed is not None and source.get('license') not in allowed:
            report['errors'].append({'code': 'license_policy', 'message': 'Declared license does not match the supplied allowed-license policy.'})
        report['technical_status'] = 'invalid' if report['errors'] else 'valid_with_warnings' if report['warnings'] else 'valid'
        return report
    except Exception as error:
        return {'schema_version': VERSION, 'inspector': 'loop-blender-asset-quality-v1',
                'blender_version': bpy.app.version_string, 'source': source,
                'technical_status': 'invalid', 'errors': [{'code': 'source_import_failed', 'message': str(error)}],
                'warnings': [], 'requires_suitability_review': True, 'limitations': LIMITATION}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--request', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(sys.argv[sys.argv.index('--') + 1:])
    report = inspect_source(json.loads(args.request.read_text()))
    atomic_json(args.output, report)
    print(json.dumps({'report': str(args.output), 'technical_status': report['technical_status'],
                      'errors': len(report['errors']), 'warnings': len(report['warnings'])}), flush=True)


if __name__ == '__main__':
    main()
