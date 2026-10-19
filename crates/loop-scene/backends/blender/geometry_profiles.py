"""Bounded declarative geometry for shapes that have no catalog asset.

These operations express construction, never pretend to be scanned models.
All coordinates use the node-local meter frame. No model-authored Python runs.
"""
import json
import math

import bpy
import assets
from mathutils import Vector

RECIPES = {"mesh", "extrusion", "lathe", "sweep", "heightfield"}
MAX_VERTICES = 250_000
MAX_FACES = 500_000


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError("geometry coordinates must be finite numbers")
    return float(value)


def vectors(values, dimensions, minimum, maximum=MAX_VERTICES):
    if not isinstance(values, list) or not minimum <= len(values) <= maximum:
        raise ValueError(f"expected {minimum}..{maximum} coordinate vectors")
    result = []
    for row in values:
        if not isinstance(row, (list, tuple)) or len(row) != dimensions:
            raise ValueError(f"expected {dimensions} coordinates per vector")
        result.append(tuple(number(x) for x in row))
    return result


def integer(value, minimum, maximum):
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise ValueError(f"expected integer within {minimum}..{maximum}")
    return value


def _profile(parameters):
    profile = vectors(parameters.get("profile"), 2, 3, 4096)
    if profile[0] == profile[-1]:
        profile.pop()
    if len(profile) < 3:
        raise ValueError("profile requires three distinct vertices")
    area = sum(a[0]*b[1] - b[0]*a[1] for a, b in zip(profile, profile[1:] + profile[:1]))
    if abs(area) < 1e-10:
        raise ValueError("profile has zero signed area")
    return profile if area > 0 else list(reversed(profile))


def _extrusion(p):
    profile = _profile(p)
    height = number(p.get("height"))
    base = number(p.get("base_z", 0))
    if height <= 0:
        raise ValueError("extrusion height must be positive")
    n = len(profile)
    vertices = [(x, y, z) for z in (base, base + height) for x, y in profile]
    faces = [tuple(reversed(range(n))), tuple(range(n, 2*n))]
    faces.extend((i, (i+1)%n, (i+1)%n+n, i+n) for i in range(n))
    return vertices, faces


def _lathe(p):
    profile = vectors(p.get("profile"), 2, 2, 4096)
    segments = integer(p.get("segments", 64), 8, 512)
    if len(profile) * segments > MAX_VERTICES:
        raise ValueError("lathe exceeds vertex budget")
    if any(r < 0 for r, z in profile):
        raise ValueError("lathe radii must be nonnegative")
    vertices, rings, faces = [], [], []
    for radius, z in profile:
        if radius < 1e-10:
            rings.append([len(vertices)])
            vertices.append((0, 0, z))
        else:
            rings.append(list(range(len(vertices), len(vertices)+segments)))
            vertices.extend((radius*math.cos(i*2*math.pi/segments),
                             radius*math.sin(i*2*math.pi/segments), z) for i in range(segments))
    for lower, upper in zip(rings, rings[1:]):
        if len(lower) == len(upper) == 1:
            continue
        for i in range(segments):
            j = (i+1) % segments
            if len(lower) == 1:
                faces.append((lower[0], upper[j], upper[i]))
            elif len(upper) == 1:
                faces.append((lower[i], lower[j], upper[0]))
            else:
                faces.append((lower[i], lower[j], upper[j], upper[i]))
    if p.get("cap_ends", True):
        if len(rings[0]) > 1:
            faces.append(tuple(reversed(rings[0])))
        if len(rings[-1]) > 1:
            faces.append(tuple(rings[-1]))
    return vertices, faces


def _sweep(p):
    path = [Vector(v) for v in vectors(p.get("path"), 3, 2, 4096)]
    profile = _profile(p)
    closed = p.get("closed", False)
    if not isinstance(closed, bool):
        raise ValueError("sweep closed must be boolean")
    if len(path)*len(profile) > MAX_VERTICES:
        raise ValueError("sweep exceeds vertex budget")
    if closed and len(path) < 3:
        raise ValueError("closed sweep requires three path points")
    if any((b-a).length < 1e-8 for a, b in zip(path, path[1:])):
        raise ValueError("sweep has coincident path points")
    vertices = []
    previous_tangent, normal = None, None
    for i, point in enumerate(path):
        if closed:
            tangent = path[(i+1)%len(path)] - path[i-1]
        elif i == 0:
            tangent = path[1] - point
        elif i == len(path)-1:
            tangent = point - path[i-1]
        else:
            tangent = path[i+1] - path[i-1]
        if tangent.length < 1e-8:
            raise ValueError("sweep path reverses at a vertex")
        tangent.normalize()
        if normal is None:
            reference = Vector((0, 0, 1)) if abs(tangent.z) < .9 else Vector((0, 1, 0))
            normal = reference.cross(tangent).normalized()
        else:
            normal = previous_tangent.rotation_difference(tangent) @ normal
            normal = (normal - tangent*normal.dot(tangent)).normalized()
        binormal = tangent.cross(normal).normalized()
        vertices.extend(tuple(point + normal*u + binormal*v) for u, v in profile)
        previous_tangent = tangent
    n = len(profile)
    faces = []
    for i in range(len(path) if closed else len(path)-1):
        k = (i+1) % len(path)
        faces.extend((i*n+j, i*n+(j+1)%n, k*n+(j+1)%n, k*n+j) for j in range(n))
    if not closed:
        faces.extend((tuple(reversed(range(n))), tuple(range((len(path)-1)*n, len(path)*n))))
    return vertices, faces


def _heightfield(p, node):
    heights = p.get("heights")
    if not isinstance(heights, list) or not 2 <= len(heights) <= 1024:
        raise ValueError("heightfield needs 2..1024 rows")
    if not isinstance(heights[0], list) or not 2 <= len(heights[0]) <= 1024:
        raise ValueError("heightfield needs 2..1024 columns")
    rows, columns = len(heights), len(heights[0])
    if rows*columns > MAX_VERTICES:
        raise ValueError("heightfield exceeds vertex budget")
    if any(not isinstance(row, list) or len(row) != columns for row in heights):
        raise ValueError("heightfield rows must have equal length")
    low, high = node["bounds_local"]["min"], node["bounds_local"]["max"]
    vertices = [(low[0] + x*(high[0]-low[0])/(columns-1),
                 low[1] + y*(high[1]-low[1])/(rows-1), number(heights[y][x]))
                for y in range(rows) for x in range(columns)]
    faces = []
    for y in range(rows-1):
        for x in range(columns-1):
            a = y*columns+x
            # Triangulate explicitly so non-planar cells export consistently.
            faces.extend(((a, a+1, a+columns+1), (a, a+columns+1, a+columns)))
    return vertices, faces


def generate(node, root, material, material_factory):
    recipe = node["generation"]["recipe"]
    p = node["generation"].get("parameters", {})
    if recipe == "mesh":
        vertices = vectors(p.get("vertices"), 3, 3)
        faces = p.get("faces")
    elif recipe == "extrusion":
        vertices, faces = _extrusion(p)
    elif recipe == "lathe":
        vertices, faces = _lathe(p)
    elif recipe == "sweep":
        vertices, faces = _sweep(p)
    else:
        vertices, faces = _heightfield(p, node)
    if not isinstance(faces, list) or not 1 <= len(faces) <= MAX_FACES:
        raise ValueError("invalid face count")
    checked = []
    index_count = 0
    for face in faces:
        if not isinstance(face, (list, tuple)) or not 3 <= len(face) <= 4096:
            raise ValueError("faces need 3..4096 vertex indices")
        indices = [integer(index, 0, len(vertices)-1) for index in face]
        index_count += len(indices)
        if index_count > 2_000_000:
            raise ValueError("geometry exceeds face-index budget")
        if len(set(indices)) != len(indices):
            raise ValueError("face has duplicate vertex indices")
        # A distinct index list can still form a collapsed face. Measure the
        # polygon area vector before letting it reach export/render operations.
        origin = Vector(vertices[indices[0]])
        area = Vector((0, 0, 0))
        for i in range(1, len(indices)-1):
            area += (Vector(vertices[indices[i]])-origin).cross(Vector(vertices[indices[i+1]])-origin)
        if area.length < 1e-12:
            raise ValueError("face has zero geometric area")
        checked.append(indices)
    low, high = node["bounds_local"]["min"], node["bounds_local"]["max"]
    if any(not low[i] - 1e-5 <= v[i] <= high[i] + 1e-5 for v in vertices for i in range(3)):
        raise ValueError(f"{recipe} vertices escape declared local bounds")
    mesh = bpy.data.meshes.new(node["node_id"] + ".geometry")
    mesh.from_pydata(vertices, [], checked)
    mesh.update()
    if mesh.validate(verbose=False):
        bpy.data.meshes.remove(mesh)
        raise ValueError("invalid geometry topology; Blender repaired the mesh")
    obj = bpy.data.objects.new(node["node_id"] + "." + recipe, mesh)
    bpy.context.scene.collection.objects.link(obj)
    obj.parent = root
    for spec in node.get("materials", []):
        mesh.materials.append(material_factory(spec))
    if not mesh.materials:
        mesh.materials.append(material)
    if p.get('material_asset_id'):
        assets.apply_surface([obj], p, root)
    indices = p.get("material_indices")
    if indices is not None:
        if not isinstance(indices, list) or len(indices) != len(mesh.polygons):
            raise ValueError("material_indices must match face count")
        for polygon, index in zip(mesh.polygons, indices):
            polygon.material_index = integer(index, 0, len(mesh.materials)-1)
    if p.get("smooth", recipe in {"lathe", "sweep", "heightfield"}):
        for polygon in mesh.polygons:
            polygon.use_smooth = True
    obj["node_id"] = node["node_id"]
    obj["semantic_kind"] = node["kind"]
    root["geometry_evidence"] = json.dumps({"node_id": node["node_id"],
        "quality_class": "authored_geometry", "recipe": recipe,
        "vertices": len(vertices), "faces": len(checked)})
    return [obj]
