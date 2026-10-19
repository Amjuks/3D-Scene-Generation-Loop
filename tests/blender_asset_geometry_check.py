"""Real Blender ingestion, instancing, fidelity evidence and geometry regression.

Run: blender --background --factory-startup --disable-autoexec
     --python-exit-code 31 --python tests/blender_asset_geometry_check.py
The tiny generated fixture tests import correctness, not photorealistic quality.
"""
import copy
import hashlib
import json
import math
import sys
import tempfile
from pathlib import Path

import bpy

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "crates/loop-scene/backends/blender"))
import scene_backend as worker


def node(recipe, parameters=None, name=None):
    return {"node_id": name or recipe, "kind": "semantic_custom_shape", "revision": 1,
            "transform": {"translation": [0, 0, 0], "rotation_xyzw": [0, 0, 0, 1]},
            "bounds_local": {"min": [-3, -3, 0], "max": [3, 3, 4]},
            "generation": {"recipe": recipe, "strategy": "recipe", "parameters": parameters or {}},
            "materials": [{"material_id": "test", "base_color": [.2, .3, .4, 1]}]}


def generate(n, reset=True):
    if reset:
        worker.reset()
    return worker.geometry(n, worker.node_root(n))


def rejected(n, expected):
    try:
        generate(n)
    except (ValueError, FileNotFoundError) as error:
        assert expected in str(error), str(error)
    else:
        raise AssertionError(f"invalid geometry accepted: {n}")


parameters = {
    "mesh": {"vertices": [[-1,-1,0],[1,-1,0],[0,1,0],[0,0,2]],
             "faces": [[2,1,0],[0,1,3],[1,2,3],[2,0,3]], "material_indices": [0]*4},
    "extrusion": {"profile": [[-2,-2],[2,-2],[2,0],[0,0],[0,2],[-2,2]], "height": 3},
    "lathe": {"profile": [[0,0],[.8,0],[1,1],[.6,2],[.8,3],[0,3]], "segments": 32},
    "sweep": {"path": [[-2,0,1],[0,0,2],[2,0,1]],
              "profile": [[-.15,-.15],[.15,-.15],[.15,.15],[-.15,.15]]},
    "heightfield": {"heights": [[0,.3,0],[.2,2,.4],[0,.2,0]]},
}
for recipe, params in parameters.items():
    made = generate(node(recipe, params))
    report = worker.inspect_scene()
    assert report["nonempty"] and report["triangles"] > 0, recipe
    assert len(made) == 1 and report["geometry_evidence"][0]["quality_class"] == "authored_geometry", report
    assert all(report["bounds_canonical"]["min"][i] >= [-3,-3,0][i] - 1e-5 and
               report["bounds_canonical"]["max"][i] <= [3,3,4][i] + 1e-5 for i in range(3)), report
    print("DECLARATIVE GEOMETRY PASSED:", recipe, flush=True)

rejected(node("unknown_tree_sculpture"), "unsupported recipe")
bad = copy.deepcopy(parameters["mesh"]); bad["faces"][0] = [0,1,500]
rejected(node("mesh", bad), "expected integer")
bad = copy.deepcopy(parameters["mesh"]); bad["vertices"][0][0] = float("nan")
rejected(node("mesh", bad), "finite numbers")
bad = copy.deepcopy(parameters["mesh"]); bad["vertices"][0][0] = -4
rejected(node("mesh", bad), "escape declared")
rejected(node("mesh", {"vertices":[[0,0,0],[1,0,0],[2,0,0]],"faces":[[0,1,2]]}), "zero geometric area")
rejected(node("extrusion", {"profile": [[0,0],[1,0],[2,0]], "height": 1}), "zero signed area")
rejected(node("lathe", {"profile": [[-1,0],[1,1]]}), "nonnegative")
rejected(node("heightfield", {"heights": [[0,0],[0]]}), "equal length")
rejected(node("sweep", {"path": [[0,0,0],[0,0,0]], "profile": [[0,0],[1,0],[0,1]]}), "coincident")
rejected(node("asset", {"asset_path": "/missing.glb", "asset_sha256": "0"*64}), "missing.glb")
rejected(node("slab", {"component_glb_path": "/missing-component.glb"}), "missing-component.glb")

with tempfile.TemporaryDirectory(prefix="loop-assets-") as tmp:
    tmp = Path(tmp)
    worker.reset()
    bpy.ops.mesh.primitive_cube_add(size=1, location=(10,20,3.5))
    source = bpy.context.object
    source.dimensions = (2,1,3)
    bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
    material = worker.mat({"material_id": "source_material", "base_color": [.9,.1,.05,1], "roughness": .23})
    source.data.materials.append(material)
    image = bpy.data.images.new("packed_source_texture", width=8, height=8)
    image.generated_color = (.9,.1,.05,1)
    image.pack()
    texture = material.node_tree.nodes.new("ShaderNodeTexImage"); texture.image = image
    material.node_tree.links.new(texture.outputs["Color"], material.node_tree.nodes["Principled BSDF"].inputs["Base Color"])
    blend, glb = tmp/"source.blend", tmp/"source.glb"
    bpy.ops.wm.save_as_mainfile(filepath=str(blend))
    bpy.ops.export_scene.gltf(filepath=str(glb), export_format="GLB", export_yup=True)
    for source_path in (glb, blend):
        sha = hashlib.sha256(source_path.read_bytes()).hexdigest()
        n = node("asset", {"asset_path": str(source_path), "asset_sha256": sha,
                           "asset_id": "test-fixture", "asset_license": "generated-local"})
        first = generate(n)
        report = worker.inspect_scene()
        low, high = report["bounds_canonical"]["min"], report["bounds_canonical"]["max"]
        assert abs(low[2]) < 1e-5 and abs(high[2]-4) < 1e-5, report
        assert abs((high[0]-low[0])/(high[1]-low[1])-2) < 1e-5, report
        assert "source_material" in report["materials"], report
        assert any(x.image for x in first[0].data.materials[0].node_tree.nodes if x.type == "TEX_IMAGE")
        evidence = report["geometry_evidence"][0]
        assert evidence["source_sha256"] == sha and evidence["uniform_scale"] == 4/3, evidence
        duplicate = copy.deepcopy(n); duplicate["node_id"] = "second"; duplicate["transform"]["translation"] = [20,0,0]
        second = generate(duplicate, reset=False)
        assert first[0].data is second[0].data, "instances duplicated source meshes"
        report = worker.inspect_scene()
        assert report["mesh_count"] == 2 and report["unique_mesh_count"] == 1, report
        assert any(e["node_id"] == "second" and e["linked_instance"] for e in report["geometry_evidence"]), report
        assert abs(second[0].matrix_world.translation.x-first[0].matrix_world.translation.x-20) < 1e-5
        print("VERIFIED ASSET IMPORT + LINKED INSTANCES PASSED:", source_path.suffix, flush=True)
        broken = copy.deepcopy(n); broken["generation"]["parameters"]["asset_sha256"] = "0"*64
        rejected(broken, "SHA-256 mismatch")
        selected = copy.deepcopy(n); selected['generation']['parameters']['asset_object'] = 'Cube'
        assert len(generate(selected)) == 1
        selected['generation']['parameters']['asset_object'] = 'missing_variant'
        rejected(selected, "not found uniquely")

        terrain = node('heightfield', {'heights': [[0,.3],[.2,.1]],
            'material_asset_id': 'fixture-surface', 'material_asset_path': str(source_path),
            'material_asset_sha256': sha, 'texture_scale_m': 2})
        surface = generate(terrain)[0]
        assert worker.inspect_scene()['mesh_count'] == 1, 'source swatch leaked into output'
        assert surface.data.uv_layers and surface.data.materials[0].name.startswith('source_material')
        duplicate = copy.deepcopy(terrain); duplicate['node_id'] = 'other-terrain'
        assert generate(duplicate, reset=False)[0].data.materials[0] == surface.data.materials[0]
        print('VERIFIED SURFACE MATERIAL AND METRIC UV PASSED', flush=True)
        for recipe in ['slab','wall','window','door','stairs']:
            structural = node(recipe, {k:v for k,v in terrain['generation']['parameters'].items() if k != 'heights'})
            made = generate(structural)
            finished = [o for o in made if any(m and m.name.startswith('source_material') for m in o.data.materials)]
            assert finished, recipe + ' silently ignored source finish'
            assert all(o.data.uv_layers for o in finished), recipe
            assert all(any(n.type=='TEX_IMAGE' and n.image for m in o.data.materials if m and m.use_nodes for n in m.node_tree.nodes) for o in finished)
            if recipe == 'window':
                assert any(m and m.name.startswith('glass') for o in made for m in o.data.materials), 'finish replaced glass'
        print('STRUCTURAL SOURCE FINISHES, METRIC UVS AND GLASS PRESERVATION PASSED',flush=True)

    # Component normalization is applied once; assembly preserves the canonical
    # bounds and uses linked meshes when the promoted component appears twice.
    component_dir = tmp/"component"
    worker.component({"node": node("asset", {"asset_path": str(glb),
        "asset_sha256": hashlib.sha256(glb.read_bytes()).hexdigest(), "asset_id": "fixture"}),
        "output_dir": str(component_dir)})
    promoted = node("asset", {"component_glb_path": str(component_dir/"component.glb")})
    first = generate(promoted)
    duplicate = copy.deepcopy(promoted); duplicate["node_id"] = "duplicate"
    duplicate["transform"]["translation"] = [20,0,0]
    second = generate(duplicate, reset=False)
    report = worker.inspect_scene()
    assert first[0].data is second[0].data
    assert {e["node_id"] for e in report["geometry_evidence"]} == {"asset", "duplicate"}, report
    assert all(e["quality_class"] == "catalog_asset" for e in report["geometry_evidence"]), report
    assert abs(report["bounds_canonical"]["max"][2]-4) < 1e-5
    print("PROMOTED COMPONENT PROVENANCE + INSTANCING PASSED", flush=True)

print("ALL ASSET AND DECLARATIVE GEOMETRY CHECKS PASSED", flush=True)
