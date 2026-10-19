# Offline production workflow

The target is full-detail offline worlds. Original source meshes and textures
are retained by default. Low-cost review renders and final delivery settings are
separate: `production-offline.json` reviews at 1280×960 / 64 samples and saves
the native scene with 3840×2160 / 1024-sample Cycles settings. Final images are
not automatically rendered at that expense. Configure the render machine's GPU
and choose the appropriate final camera in Blender.

```bash
target/debug/loop scene run --config examples/adaptive/production-offline.json \
  --tasks examples/adaptive/tasks.json
```

This profile permits 100 million evaluated triangles and 16 GB of texture data.
Those are circuit breakers, not detail targets or evidence the host has enough
memory. Geometry is not simplified to meet them. Required visual review warnings
must be resolved before acceptance (`require_no_visual_warnings: true`). Review
acceptance is evidence about supplied views, not a guarantee of perfect realism.

## Source components and surface finishes

Use artist-authored or scanned sources for the actual subject, species, era,
construction and use. The importer checks source SHA-256, runs mesh/UV/texture
inspection once per unique import, and preserves linked geometry and materials.
Technical failures stop import. Open foliage surfaces are reported for review,
not indiscriminately rejected as non-manifold. Reports do not certify artistic
or botanical accuracy.

```bash
python3 scripts/scene_assets.py search 'pine sapling' --provider all \
  --catalog scene-assets/catalog.json --identity pine --must sapling
python3 scripts/scene_assets.py search 'clay plaster' --type textures \
  --provider all --catalog scene-assets/catalog.json
python3 scripts/scene_assets.py fetch clay_plaster --type textures \
  --catalog scene-assets/catalog.json --resolution 4k --max-bytes 250000000 \
  --id polyhaven:clay_plaster-4k --material-family 'clay plaster' \
  --surface-use architectural_wall --surface-use exhibition_screen
python3 scripts/inspect_scene_asset.py --catalog scene-assets/catalog.json \
  --asset-id polyhaven:pine_sapling_small_a --output scene-assets/qa/pine.json \
  --blender /path/to/blender
```

Search only produces candidates with matching evidence. It does not rename a
similar object to satisfy the scene. Supported provider resolution selections
are 1k, 2k, 4k, 8k and 16k; the selected source must actually offer that glTF
variant. An unavailable resolution fails explicitly. Existing IDs cannot be
silently overwritten with different source bytes; use a new catalogue/ID for a
different resolution. No missing displacement or microgeometry is invented.

Local `add` accepts `--kind model|material`, `--material-family`, and repeated
`--surface-use`. For example, restrict forest soil to `terrain` and
`landscape_ground`, plaster to `architectural_wall` and `exhibition_screen`.
Plans must supply the exact compatible `parameters.material_usage`. A material
swatch cannot be used as a model. An explicitly classified model cannot be used
as a material swatch; curate its finish as a separate material source if needed.
Unrestricted legacy entries still require visual suitability review.

## Missing components

Adaptive planning saves `designs/adaptive/asset-requirements-*.json` before
waiting on absent sources. Every domain can supply registered components, but
the finite provider library does not cover every domain. Track and resolve gaps:

```bash
python3 scripts/scene_asset_requirements.py init \
  --input /path/to/run/designs/adaptive/asset-requirements-program.json \
  --output /path/to/requirements.json
python3 scripts/scene_asset_requirements.py search \
  --manifest /path/to/requirements.json --catalog scene-assets/catalog.json \
  --provider all
python3 scripts/scene_asset_requirements.py bind REQUIREMENT_ID ASSET_ID \
  --manifest /path/to/requirements.json --catalog scene-assets/catalog.json \
  --reason 'Inspected exact subject, scale and intended use'
python3 scripts/scene_asset_requirements.py verify \
  --manifest /path/to/requirements.json --catalog scene-assets/catalog.json
```

Register the selected exact source, inspect it, then resume the scene. Binding
does not automatically change a scene plan or guarantee its eventual appearance.
The manifest retains provenance and detects changed bytes. Metadata matching
cannot establish anatomical accuracy or culturally specific design by itself.

## Scene-authored lighting and delivery

The scene director authors `environment` on the scene program: purpose,
constant world or physical sky, exposure, an optional sun, and up to 64 explicit
area/point/spot sources. Light position and target are world metres; sun azimuth
is measured from +X toward +Y. No mandatory area fill or ceiling lamps are added
to adaptive scenes. Missing environment on historical scenes retains their
legacy lighting. Inspection reports record the selected environment.

The native `.blend` is authoritative for offline lighting. glTF carries
punctual lights but does not carry Blender world shaders or area lights; a
viewer needs its own environment setup. See the
[Blender glTF documentation](https://docs.blender.org/manual/en/latest/addons/import_export/scene_gltf2.html).

`quality.preserve_source_detail` defaults to true and rejects `asset_lod`.
Optional LOD is for separate explicitly authorized derivatives: set that policy
false and request `parameters.asset_lod` with ratio and measured bounds/surface
error limits. Derivatives never overwrite sources; their hashes and sampling
limits are recorded. They still require visual review. No model-driven default
requests LOD. Exact linked instances, source caches, and contact-ray pruning
provide efficiency without removing source geometry.

Huge-world streaming, rich cross-domain component coverage, geometry-aware UV
unwraps/displacement workflows and a broad high-detail visual benchmark remain
open work. Increasing limits does not implement these capabilities.

## Dense source distributions without proxy geometry

An adaptive asset Object can include `distribution` beside `size`, `support`
and `parameters`. `size` describes one real source model. The compiler expands
the distribution into ordinary owned scene nodes before validation:

```json
{
  "count": 40,
  "polygon": [[-10,-10],[10,-10],[8,9],[-7,10]],
  "exclusions": [[[-1,-10],[1,-10],[1,10],[-1,10]]],
  "min_spacing_m": 1.5,
  "seed": 19,
  "yaw_degrees": [0,360],
  "uniform_scale": [1,1]
}
```

Polygons are region-local XY. Each instance retains the prototype's base Z or
uses its declared support. Only upright source assets can be distributed.
The source's conservative horizontal footprint stays inside the boundary and
outside exclusions. Spacing is between centres across all distributed source prototypes in the
region, including different variants; each pair respects the larger requested
spacing. It does not prove mesh collision clearance or spacing from unrelated
manually placed objects. A shared spatial index keeps spacing checks local. Counts, seed, polygon, exclusions and physical
spacing are deterministic; an impossible density fails instead of dropping
objects. Every instance counts toward regional and global node budgets.

Use multiple actual source variants for morphological variation. Random yaw or
scaling does not make duplicate trees into distinct botanical models. Default
uniform scale is exactly one. All meshes remain at full detail and identical
sources share mesh/material data. The 48-sapling validation task is
`distributed-landscape-task.json`; it needs registered single-tree variants
`pine_sapling_small_a`, `_b`, `_c`, `rock_09`, and `forest_ground_04`.

Camera repairs now patch views by ID and preserve omitted views. Environment
patches merge into the current settings, then validate the whole result.
Unchanged or invalid proposals are returned for bounded correction before
another render is spent. Source-material UV application covers structural
recipes as well as explicit profiles; glass/hardware remain separate.
Component cache keys include the actual Blender worker-source fingerprint.


## Recovering an existing category batch

Inspect `progress.md` and the individual logs first. Source gaps and spending
limits now stop automatic retries instead of triggering identical work. After
supplying missing sources, use `--retry-asset-blocked`. Keep original run inputs
immutable when changing production targets or model configuration:

```bash
python3 scene_batch.py --resume /path/to/batch \
  --recover-config /path/to/production-config.json --dry-run
python3 scene_batch.py --resume /path/to/batch --workers 2 --max-attempts 10
```

The first command saves a versioned config and queues fresh attempts for unfinished
scenes. It preserves original prompts, accepted outputs, archived attempts and
lifetime attempt counters. Choose an explicit total attempt limit appropriate to
the existing count; this example permits two more attempts after eight failures.
A task may carry explicit `overrides`, for example intentional voxel style with
image review, while other tasks use realistic fidelity. Configuration recovery
does not procure missing components or certify the output.

Program connections can specify one shared `world_at` point inside both region
envelopes. Legacy local `at`/`target_at` pairs remain supported; do not mix the two
representations. Validation reports every bad connection with numerical evidence.
Visual repairs use compact authoring data and changed-object patches. Request-
hashed raw model responses survive schema errors for safe reparse after a parser
fix; historical logs without a matching request hash remain diagnostic evidence.

Exhausted response-schema corrections stop the batch task; fix the exact reported
field or model contract before using `--retry-schema-blocked`. Image-review schema
correction keeps the original images attached and has one bounded attempt. Its
raw checkpoint includes actual image bytes, scene context and model settings;
replay does not add duplicate token charges or infer missing coverage as passing.

Program planning records its catalogue context so later source additions do not
force a new spatial design. Dependency review uses the current catalogue and can
bind a suitable exact source ID to existing region owners. Regional compilation
requires those owners to actually use the bound source; normal source hashes,
license/type/use checks and final visual review still apply. Custom fabrication
is reviewed separately from real anatomical/species/scan dependencies. An authored
construction plan alone is never evidence of production-quality appearance.
