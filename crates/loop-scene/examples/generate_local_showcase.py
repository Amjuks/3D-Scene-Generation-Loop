"""Generate local showcase scenes when the Soket design service is unavailable.

This intentionally exercises the same isolated Blender worker and cross-format
validator as the controller. It does not claim LLM or visual-review acceptance.
"""
import json
import math
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
BLENDER = Path("/home/aman/3d_scene_generation/.cache/tools/blender-4.5.1-linux-x64/blender")
BACKEND = ROOT / "crates/loop-scene/backends/blender/scene_backend.py"
OUTPUT = ROOT / "showcase-scenes"


def material(name, color, roughness=0.45, metallic=0.0):
    return {"material_id": name, "description": name.replace("_", " "),
            "base_color": color, "metallic": metallic, "roughness": roughness,
            "texture_scale_m": 0.7, "texture_ref": None}


def node(node_id, kind, bounds, at=(0, 0, 0), recipe=None, mat=None,
         rotation=(0, 0, 0, 1), construction=None):
    return {
        "schema_version": 1, "node_id": node_id, "revision": 1,
        "kind": kind, "parent_id": "scene", "child_ids": [],
        "purpose": f"Showcase {kind}", "required_features": ["credible construction", "grounded contact"],
        "assumptions": ["Standard real-world dimensions"],
        "design": f"Detailed {kind} for a realistic interior.",
        "construction_instructions": "Use believable thickness, softened edges, and physical contact.",
        "units": "m",
        "transform": {"translation": list(at), "rotation_xyzw": list(rotation), "scale": [1, 1, 1]},
        "pivot_local": [0, 0, 0], "local_front": [0, -1, 0], "local_up": [0, 0, 1],
        "bounds_local": {"min": list(bounds[0]), "max": list(bounds[1])},
        "usable_volume_local": None, "child_regions": [], "placement_region": None,
        "interfaces": [], "relationships": [], "construction": construction or {},
        "materials": [mat or material("warm_oak", [0.34, 0.16, 0.055, 1], 0.4)],
        "generation": {"strategy": "recipe", "recipe": recipe or kind,
                       "parameters": {}, "seed": 42, "detail_policy": "high",
                       "required_capabilities": ["pbr_materials"]},
        "acceptance": {"numeric_rules": {"contact_tolerance_m": 0.005},
                       "visual_requirements": ["believable proportions", "coherent material scale"],
                       "review_views": ["exterior_three_quarter", "overhead", "interior"],
                       "evidence_refs": []},
        "provenance": {"owning_task": "local-showcase", "parent_revision": 1,
                       "dependency_hashes": [], "model": "local-authored",
                       "prompt_version": "local-showcase-v1", "recipe_version": "procedural_pbr_v1",
                       "source_url": None, "creator": "Loop local backend",
                       "license": "generated-local", "validation_status": "pending"}
    }


def scene(prompt, children):
    root = node("scene", "room", ((-4, -3, -0.15), (4, 3, 3.25)), recipe="assembly")
    root["parent_id"] = None
    root["child_ids"] = [x["node_id"] for x in children]
    root["generation"]["strategy"] = "assembly"
    root["purpose"] = prompt
    return {"schema_version": 1, "root_id": "scene", "prompt_summary": prompt,
            "coordinate_system": "RH_M_ZUP_XEAST_YNORTH", "nodes": [root] + children}


WOOD = material("quarter_sawn_oak", [0.32, 0.145, 0.045, 1], 0.38)
WALNUT = material("dark_walnut", [0.115, 0.045, 0.018, 1], 0.34)
FABRIC = material("woven_sage", [0.18, 0.28, 0.20, 1], 0.72)
PLASTER = material("warm_plaster", [0.72, 0.68, 0.59, 1], 0.82)
STONE = material("charcoal_stone", [0.09, 0.105, 0.12, 1], 0.58)
BRASS = material("aged_brass", [0.42, 0.25, 0.07, 1], 0.24, 0.75)


def shell():
    return [
        node("floor", "floor", ((-4, -3, 0), (4, 3, .12)), recipe="floor", mat=WOOD),
        node("rear_wall", "wall", ((-4, -.06, 0), (4, .06, 3.1)), at=(0, 2.92, 0), mat=PLASTER),
        node("left_wall", "wall", ((-.06, -3, 0), (.06, 3, 3.1)), at=(-3.92, 0, 0), mat=PLASTER),
        node("right_wall", "wall", ((-.06, -3, 0), (.06, 3, 3.1)), at=(3.92, 0, 0), mat=PLASTER),
    ]


def dining_scene():
    c = shell()
    c += [node("dining_table", "table", ((-1.2, -.55, 0), (1.2, .55, .78)), mat=WOOD)]
    chair_bounds = ((-.28, -.3, 0), (.28, .3, 1.02))
    for i, (x, y, angle) in enumerate([(-.72, -1.05, 0), (.72, -1.05, 0),
                                        (-.72, 1.05, math.pi), (.72, 1.05, math.pi),
                                        (-1.65, 0, -math.pi/2), (1.65, 0, math.pi/2)]):
        c.append(node(f"chair_{i+1}", "chair", chair_bounds, at=(x, y, 0), mat=FABRIC,
                      rotation=(0, 0, math.sin(angle/2), math.cos(angle/2))))
    c += [node("sideboard", "cabinet", ((-.9, -.24, 0), (.9, .24, 1.05)), at=(0, 2.55, 0), mat=WALNUT),
          node("plant", "plant", ((-.35, -.35, 0), (.35, .35, 1.65)), at=(3.15, 2.15, 0)),
          node("floor_lamp", "lamp", ((-.3, -.3, 0), (.3, .3, 1.9)), at=(-3.1, 2.0, 0), mat=BRASS)]
    return scene("A refined oak dining room for six with warm contemporary styling", c)


def reading_scene():
    c = shell()
    c += [node("library_shelves", "shelf", ((-1.65, -.22, 0), (1.65, .22, 2.55)), at=(0, 2.62, 0), mat=WALNUT, construction={"shelf_count": 6}),
          node("reading_chair", "chair", ((-.48, -.5, 0), (.48, .5, 1.12)), at=(-.65, -.15, 0), mat=FABRIC, rotation=(0, 0, .23, .973)),
          node("side_table", "table", ((-.38, -.38, 0), (.38, .38, .58)), at=(.65, -.1, 0), mat=WOOD),
          node("reading_lamp", "lamp", ((-.28, -.28, 0), (.28, .28, 1.75)), at=(1.25, .25, 0), mat=BRASS),
          node("window", "window", ((-.9, -.06, 0), (.9, .06, 1.35)), at=(-2.95, 0.7, 1.0), mat=WOOD),
          node("plant", "plant", ((-.38, -.38, 0), (.38, .38, 1.8)), at=(2.9, 2.1, 0))]
    return scene("A quiet, warmly lit reading room with built-in walnut library shelves", c)


def office_scene():
    c = shell()
    c += [node("executive_desk", "table", ((-1.25, -.62, 0), (1.25, .62, .77)), at=(0, .35, 0), mat=WALNUT),
          node("desk_chair", "chair", ((-.42, -.44, 0), (.42, .44, 1.2)), at=(0, 1.35, 0), mat=STONE, rotation=(0, 0, 1, 0)),
          node("guest_chair_1", "chair", ((-.36, -.4, 0), (.36, .4, 1.0)), at=(-.7, -.9, 0), mat=FABRIC),
          node("guest_chair_2", "chair", ((-.36, -.4, 0), (.36, .4, 1.0)), at=(.7, -.9, 0), mat=FABRIC),
          node("storage", "cabinet", ((-1.25, -.25, 0), (1.25, .25, 1.15)), at=(0, 2.55, 0), mat=WALNUT),
          node("display_shelf", "shelf", ((-.7, -.2, 0), (.7, .2, 2.25)), at=(-3.55, 1.1, 0), mat=WOOD, construction={"shelf_count": 5}),
          node("plant", "plant", ((-.4, -.4, 0), (.4, .4, 1.9)), at=(3.2, 2.1, 0)),
          node("task_lamp", "lamp", ((-.2, -.2, 0), (.2, .2, .72)), at=(.75, .35, .78), mat=BRASS)]
    return scene("A high-end executive home office in walnut, stone, and sage textiles", c)


def blender(job_path):
    subprocess.run([str(BLENDER), "--background", "--factory-startup", "--disable-autoexec",
                    "--python", str(BACKEND), "--", str(job_path)], check=True)


def generate(name, spec):
    out = OUTPUT / name
    out.mkdir(parents=True, exist_ok=True)
    (out / "scene-spec.json").write_text(json.dumps(spec, indent=2))
    job = {"mode": "assembly", "scene": spec, "output_dir": str(out), "renderer": "eevee",
           "resolution": [1280, 720], "samples": 32}
    (out / "assembly-job.json").write_text(json.dumps(job, indent=2))
    blender(out / "assembly-job.json")
    validation = out / "export-validation"
    validation.mkdir(exist_ok=True)
    validate_job = {"mode": "validate_exports", "blend": str(out / "scene.blend"),
                    "glb": str(out / "scene.glb"), "output_dir": str(validation),
                    "translation_tolerance_m": .001}
    (validation / "job.json").write_text(json.dumps(validate_job, indent=2))
    blender(validation / "job.json")
    parity = json.loads((validation / "parity.json").read_text())
    if not parity["passed"]:
        raise RuntimeError(f"{name} parity validation failed: {parity['errors']}")
    (out / "LOCAL-NOT-VISION-REVIEWED.txt").write_text(
        "Generated and cross-format validated locally. Soket and model-based visual review were unavailable.\n")
    return {"name": name, "output": str(out), "parity": True,
            "renders": [str(p) for p in sorted(out.glob("*.png"))]}


OUTPUT.mkdir(exist_ok=True)
results = [generate("dining-room", dining_scene()),
           generate("reading-room", reading_scene()),
           generate("executive-office", office_scene())]
(OUTPUT / "manifest.json").write_text(json.dumps({"scenes": results}, indent=2))
print(json.dumps({"scenes": results}, indent=2))
