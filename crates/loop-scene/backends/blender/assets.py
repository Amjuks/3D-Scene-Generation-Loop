"""Verified asset ingestion and linked mesh reuse; never execute asset scripts.

GLB imports are already converted to Blender's Z-up meter coordinates. Blend
libraries must use that canonical basis. Source geometry/materials stay intact;
fitting uses one uniform scale, never independently stretches object axes.
"""
import hashlib
import json
import math
import re
from pathlib import Path

import bpy
from mathutils import Vector
import asset_lod
import asset_quality

_CACHE = {}
_MATERIAL_CACHE = {}


def clear_cache():
    _CACHE.clear()
    _MATERIAL_CACHE.clear()


def surface_material(parameters):
    """Use a verified source material independently of its source shape."""
    path, sha = parameters.get('material_asset_path'), parameters.get('material_asset_sha256')
    if not path or not isinstance(sha, str) or len(sha) != 64:
        raise ValueError('surface material requires resolved catalog source and SHA-256')
    selection = parameters.get('material_name')
    key = (path, sha, selection)
    if key in _MATERIAL_CACHE:
        return _MATERIAL_CACHE[key]
    objects, _, _ = _load(path, sha, parameters.get('material_asset_object'))
    materials = {m for obj in objects if obj.type == 'MESH' for m in obj.data.materials if m}
    if selection:
        materials = {m for m in materials if m.name == selection or re.fullmatch(re.escape(selection) + r'\.\d{3,}', m.name)}
    if len(materials) != 1:
        raise ValueError('material source must select exactly one material using material_name')
    material = next(iter(materials))
    # Keep datablocks alive for linked import caching but omit source geometry
    # from this scene. Only the chosen material belongs to the authored surface.
    for obj in objects:
        for collection in list(obj.users_collection):
            collection.objects.unlink(obj)
    _MATERIAL_CACHE[key] = material
    return material


def apply_surface(objects, parameters, root, replace_material=None):
    """Apply a verified finish and metric UVs only to the intended base surface.

    Structural fixtures may also contain glass/hardware: those slots retain
    their materials. UVs use the node frame so adjoining pieces share density
    and alignment, including objects with local rotations or translations.
    """
    scale = parameters.get('texture_scale_m', 1.0)
    if isinstance(scale, bool) or not isinstance(scale, (int,float)) or not math.isfinite(scale) or scale <= 0:
        raise ValueError('texture_scale_m must be finite and positive')
    material = surface_material(parameters)
    bpy.context.view_layer.update()
    frame = root.matrix_world.inverted()
    applied = 0
    for obj in objects:
        if obj.type != 'MESH':
            continue
        mesh = obj.data
        slots = {i for i, value in enumerate(mesh.materials) if replace_material is None or value == replace_material}
        if not mesh.materials and replace_material is None:
            mesh.materials.append(material)
            slots = {0}
        if not slots:
            continue
        for index in slots:
            mesh.materials[index] = material
        uv = mesh.uv_layers.active or mesh.uv_layers.new(name='Metric surface UV')
        transform = frame @ obj.matrix_world
        normals = transform.to_3x3().inverted().transposed()
        for polygon in mesh.polygons:
            if polygon.material_index not in slots:
                continue
            normal = normals @ polygon.normal
            axis = max(range(3), key=lambda i: abs(normal[i]))
            axes = [i for i in range(3) if i != axis]
            for loop_index in polygon.loop_indices:
                point = transform @ mesh.vertices[mesh.loops[loop_index].vertex_index].co
                uv.data[loop_index].uv = (point[axes[0]]/scale, point[axes[1]]/scale)
            applied += 1
    if not applied:
        raise ValueError('source surface material did not apply to any generated faces')
    root['surface_material_evidence'] = json.dumps({'asset_id':parameters['material_asset_id'],
        'sha256':parameters['material_asset_sha256'],'texture_scale_m':scale,'faces':applied})
    return material


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def _load(path, expected_hash=None, object_name=None, lod=None):
    path = Path(path).resolve(strict=True)
    if path.suffix.lower() not in {".glb", ".blend"}:
        raise ValueError("assets must be self-contained .glb or .blend files")
    lod = asset_lod.settings(lod)
    key = (str(path), expected_hash, object_name, json.dumps(lod, sort_keys=True))
    if key in _CACHE:
        templates, actual_hash = _CACHE[key]
        copies = {}
        for original, parent, matrix in templates:
            copy = original.copy()
            # copy.data intentionally remains shared: repeated chairs/trees do
            # not duplicate meshes, textures, or material datablocks in memory.
            bpy.context.scene.collection.objects.link(copy)
            copies[original] = copy
        for original, parent, matrix in templates:
            copy = copies[original]
            copy.parent = copies.get(parent)
            copy.matrix_local = matrix.copy()
        return list(copies.values()), actual_hash, True
    actual_hash = digest(path)
    if expected_hash and actual_hash != expected_hash.lower():
        raise ValueError(f"asset SHA-256 mismatch: {path.name}")
    cached_lod = asset_lod.cached(actual_hash, object_name, lod) if lod else None
    before = set(bpy.context.scene.objects)
    if cached_lod is not None:
        imported = cached_lod
    elif path.suffix.lower() == ".glb":
        bpy.ops.import_scene.gltf(filepath=str(path))
    else:
        # The worker runs with --disable-autoexec. Append data, never open the
        # file's scene or run embedded Text blocks. Clear source animations and
        # drivers so they cannot depend on scripts or external state.
        with bpy.data.libraries.load(str(path), link=False) as (source, target):
            target.objects = source.objects
        for obj in target.objects:
            if obj is not None:
                bpy.context.scene.collection.objects.link(obj)
    if cached_lod is None:
        imported = list(set(bpy.context.scene.objects) - before)
    if object_name is not None and cached_lod is None:
        if not isinstance(object_name, str) or not object_name:
            raise ValueError("asset_object must name one exact source object/subtree")
        selected = [obj for obj in imported if obj.name == object_name]
        if not selected:
            # Blender adds numeric suffixes when another variant import has
            # already used the original source name in this same scene.
            selected = [obj for obj in imported if re.fullmatch(re.escape(object_name) + r"\.\d{3,}", obj.name)]
        if len(selected) != 1:
            raise ValueError(f"asset_object {object_name!r} not found uniquely in source")
        top = selected[0]
        keep = {top, *top.children_recursive}
        # Preserve the source frame when selecting a subtree whose parent is
        # deliberately excluded (e.g. one of several vegetation variants).
        bpy.context.view_layer.update()
        matrix = top.matrix_world.copy()
        top.parent = None
        top.matrix_world = matrix
        for obj in imported[:]:
            if obj not in keep:
                bpy.data.objects.remove(obj, do_unlink=True)
                imported.remove(obj)
    for obj in imported[:]:
        if obj.type in {"LIGHT", "CAMERA"}:
            bpy.data.objects.remove(obj, do_unlink=True)
            imported.remove(obj)
        else:
            obj.animation_data_clear()
    if not any(obj.type == "MESH" for obj in imported):
        raise ValueError(f"asset contains no mesh geometry: {path.name}")
    for obj in imported:
        if obj.type != "MESH":
            continue
        for material in obj.data.materials:
            if not material or not material.use_nodes:
                continue
            for shader in material.node_tree.nodes:
                if shader.type == "TEX_IMAGE" and shader.image:
                    image = shader.image
                    # Blender lazily decodes packed images: has_data can be
                    # false for a valid import until dimensions are requested.
                    if image.size[0] <= 0 or image.size[1] <= 0:
                        raise ValueError(f"asset texture is missing or unreadable: {image.name}")
                    # Make native .blend output portable as well as the GLB.
                    if image.source == "FILE" and not image.packed_file:
                        image.pack()
    bpy.context.view_layer.update()
    quality = asset_quality.inspect_loaded(imported, source={'sha256': actual_hash, 'object_name': object_name})
    if quality['errors']:
        raise ValueError('source asset failed technical inspection: ' + '; '.join(issue['message'] for issue in quality['errors']))
    quality_evidence = {key: quality[key] for key in ('schema_version','technical_status','summary','warnings','limitations')}
    for obj in imported:
        if obj.type == 'MESH':
            obj['loop_source_technical'] = json.dumps(quality_evidence, sort_keys=True)
    if lod and cached_lod is None:
        asset_lod.derive(imported, actual_hash, object_name, lod)
    imported_set = set(imported)
    # Save the canonical hierarchy before parenting it under a scene instance.
    templates = [(obj, obj.parent if obj.parent in imported_set else None,
                  obj.matrix_local.copy()) for obj in imported]
    _CACHE[key] = (templates, actual_hash)
    return imported, actual_hash, False


def measured_bounds(objects):
    bpy.context.view_layer.update()
    deps = bpy.context.evaluated_depsgraph_get()
    points = [obj.evaluated_get(deps).matrix_world @ Vector(corner)
              for obj in objects if obj.type == "MESH"
              for corner in obj.evaluated_get(deps).bound_box]
    if not points or not all(math.isfinite(v) for p in points for v in p):
        raise ValueError("asset has empty or non-finite evaluated bounds")
    return (Vector(tuple(min(p[i] for p in points) for i in range(3))),
            Vector(tuple(max(p[i] for p in points) for i in range(3))))


def import_asset(node, root, component=False):
    parameters = node.get("generation", {}).get("parameters", {})
    path = parameters.get("component_glb_path" if component else "asset_path")
    expected = None if component else parameters.get("asset_sha256")
    if not path:
        raise ValueError(f"{node['node_id']}: unresolved asset; provide a catalog asset_id")
    if not component and (not isinstance(expected, str) or len(expected) != 64
                          or any(c not in "0123456789abcdefABCDEF" for c in expected)):
        raise ValueError("external asset requires its verified SHA-256")
    selection = None if component else parameters.get("asset_object")
    objects, actual_hash, cached = _load(path, expected, selection,
                                       None if component else parameters.get('asset_lod'))
    imported = set(objects)
    if component:
        for obj in objects:
            if obj.parent not in imported:
                obj.parent = root
            if obj.get("geometry_evidence"):
                evidence = json.loads(obj["geometry_evidence"])
                evidence["node_id"] = node["node_id"]
                evidence["linked_instance"] = cached
                obj["geometry_evidence"] = json.dumps(evidence, sort_keys=True)
    else:
        low, high = measured_bounds(objects)
        lod_reports = [json.loads(obj['loop_asset_lod']) for obj in objects
                       if obj.type == 'MESH' and obj.get('loop_asset_lod')]
        if lod_reports:
            # Keep the exact source normalization/placement across LOD levels.
            # Refitting each reduced extent would introduce scale/pivot pops.
            original_bounds = lod_reports[0]['source_bounds']
            low, high = Vector(original_bounds['min']), Vector(original_bounds['max'])
        size = high - low
        target_low = Vector(node["bounds_local"]["min"])
        target_high = Vector(node["bounds_local"]["max"])
        target_size = target_high - target_low
        if any(v <= 0 or not math.isfinite(v) for v in target_size):
            raise ValueError("asset target bounds must have positive finite extents")
        scales = [target_size[i] / size[i] for i in range(3) if size[i] > 1e-9]
        if not scales:
            raise ValueError("asset has degenerate bounds")
        scale = min(scales)
        # Bottom-center alignment works for grounded assets while retaining
        # native object pivots beneath this explicit normalization transform.
        center = (low + high) * .5
        target_center = (target_low + target_high) * .5
        anchor = bpy.data.objects.new(node["node_id"] + ".asset_frame", None)
        bpy.context.scene.collection.objects.link(anchor)
        anchor.parent = root
        anchor.scale = (scale,) * 3
        anchor.location = (target_center.x - scale * center.x,
                           target_center.y - scale * center.y,
                           target_low.z - scale * low.z)
        for obj in objects:
            if obj.parent not in imported:
                obj.parent = anchor
        evidence = {
            "node_id": node["node_id"], "quality_class": "catalog_asset",
            "asset_id": parameters.get("asset_id", ""), "source_sha256": actual_hash,
            "source_object": selection,
            "source_bounds": {"min": list(low), "max": list(high)},
            "uniform_scale": scale, "linked_instance": cached,
            "source_material_count": len({m for o in objects if o.type == "MESH"
                                          for m in o.data.materials if m}),
            "source_image_count": len({shader.image for o in objects if o.type == "MESH"
                for m in o.data.materials if m and m.use_nodes
                for shader in m.node_tree.nodes if shader.type == "TEX_IMAGE" and shader.image}),
            "source_url": parameters.get("asset_source_url"),
            "license": parameters.get("asset_license"),
        }
        if lod_reports:
            evidence['source_lod'] = lod_reports[0]
        evidence['technical_inspection'] = next((json.loads(obj['loop_source_technical']) for obj in objects if obj.get('loop_source_technical')), None)
        root["geometry_evidence"] = json.dumps(evidence, sort_keys=True)
    made = []
    for obj in objects:
        if obj.type == "MESH":
            obj["node_id"] = node["node_id"]
            obj["semantic_kind"] = node["kind"]
            made.append(obj)
    return made
