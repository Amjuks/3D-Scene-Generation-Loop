# Scene pipeline requirements and implementation

## Requirements captured from this request

- The subject determines topology, architecture, landscape, contents and scale.
  A prayer space, museum, wilderness and game world must not collapse into the
  same room with different labels or colors.
- Support small object studies through large environments. Allocate work and
  detail by spatial scale and importance, with explicit resource budgets.
- Use credible construction, dimensions, support, access, negative space and
  category-specific features. Occupied rooms are appropriate only when requested.
- Preserve real model morphology and material detail. A tree must have actual
  botanical geometry; a few primitives renamed as a tree do not meet the request.
- Preserve creative freedom beyond a fixed recipe vocabulary. Allow custom
  architectural profiles, irregular meshes, terrain relief and curved forms.
- Make complexity affordable through scoped parallel roles, component reuse,
  linked meshes, bounded processes and durable checkpoints.
- Judge produced geometry and actual images, not just varied written concepts.
  Missing models or insufficient evidence must remain explicit blockers.

## What the audit found

The original compact planner selected from a small recipe catalogue, imposed
12–28 macro assemblies and automatic room furnishing, used rectangular envelopes,
and mapped generic terrain to voxel cliffs. Object dimensions were capped at
100 m and macro plans at 64 objects. The backend's unknown recipe fallback was
a beveled box; vegetation was a trunk with simple sphere-like foliage. The
declared process limits did not parallelize the controller's component loop.

The 60 scenes in `scene-runs/collection-60` contained 3,230 recipe nodes and no
external source assets. All 60 accepted reports omitted visual review. The newer
`single-room-taxonomy-60` batch had 58 accepted scenes; its saved brief explicitly
required exactly one rectangular room even for landscapes and voxel worlds.
That brief must not be reused for unrestricted scenes. Existing artifacts and
saved jobs are preserved.

## New execution path

`planning.mode: adaptive` is the default for new configurations:

```text
request + source catalogue
  → scene program (topology, features, region envelopes, scale/detail budgets)
  → independent structure/program review
  → concurrent scoped regional designers
  → local geometry/ownership/seam validation and bounded regional repairs
  → verified source-model resolution and fidelity checks
  → bounded component generation and exact geometry reuse
  → assembly, linked source meshes and authored inspection cameras
  → geometry, export and actual-image acceptance
```

Regions are invisible work allocations, not automatically generated rooms.
The model chooses their number and purpose. Feature ownership connects the user
request to regional objects; matching strings alone is not visual proof.
Regional checkpoints include model/configuration/catalogue context and retain
successful paid work when another specialist fails. The controller remains the
only authority that validates and accepts results. The specialized roles use
Loop's model/provider, telemetry and budget facilities; they do not receive
unrestricted coding tools or execute model-written Python.

The new geometry vocabulary supports `asset`, `mesh`, `extrusion`, `lathe`,
`sweep` and `heightfield`, plus simple structural recipes and explicit voxel
style. Geometry is data, with finite coordinates, bounds and complexity checks.
Unknown recipes now fail rather than silently becoming boxes. Custom geometry
is eligible for image review, not automatically certified as realistic.

An object's optional `support` names a heightfield, slab or lathe in its region. The
compiler samples the actual triangulated surface at the object's XY position,
accounts for support rotation and the object's rotated lower bound, and resolves
its initial height. Blender then refines vertical contact against the actual
imported mesh vertices and support surface; the controller persists the measured
placement. This removes guessed contact heights without inventing a ground plane.
Vertex sampling is not full mesh collision or a proof that every root, foot or
irregular rock surface contacts the terrain. Explicit
placement remains necessary for hanging objects and other support shapes.

The source catalogue is separate from scene-local generated caches. An asset
is selected by its exact ID, checked against allowed licenses and SHA-256, and
imported with original materials and textures. Fitting uses uniform scale to
preserve proportions and aligns the model's bottom with its placement. Repeated
imports share mesh data in assembly. Catalogue `production` is a curator's
quality declaration; visual review still decides whether its use fits the scene.

Authored meshes, terrain and profile geometry can use a verified source surface
through `parameters.material_asset_id`, with optional `material_name` for a
source containing multiple materials. `texture_scale_m` controls metric UV
tiling. Source swatch geometry is excluded from the scene; its PBR material and
packed images are retained. This supports scanned surface detail without a
hardcoded palette or a different procedural shader for every scene category.

## Prepare and run

From the repository root:

```bash
cargo build -p loop-cli --locked
python3 scripts/scene_assets.py search "pine tree"
python3 scripts/scene_assets.py fetch pine_sapling_small rock_09 chinese_armchair \
  --catalog scene-assets/catalog.json --resolution 1k --max-bytes 50000000
python3 scripts/scene_assets.py fetch forest_ground_04 --type textures \
  --catalog scene-assets/catalog.json --resolution 1k --max-bytes 15000000
python3 scripts/scene_assets.py variant polyhaven:pine_sapling_small \
  --catalog scene-assets/catalog.json --id polyhaven:pine_sapling_small_a \
  --name "Pine Sapling Small A (single tree)" --object-name pine_sapling_small_a \
  --dimensions-m 0.7876736224 0.7274213135 1.2989574084
target/debug/loop scene doctor --config examples/adaptive/config.json
target/debug/loop scene run --config examples/adaptive/config.json \
  --tasks examples/adaptive/tasks.json
```

The fetch command explicitly downloads each named model and its PBR resources,
verifies provider checksums and aggregate size limits, and packs self-contained
GLBs without changing the mesh. Downloads do not happen secretly during planning.
It uses the [Poly Haven API](https://polyhaven.com/our-api); the downloaded assets
are [CC0](https://polyhaven.com/license). Powered by Poly Haven.

For your own self-contained GLB or packed Blender file:

```bash
python3 scripts/scene_assets.py add /absolute/path/to/model.glb \
  --catalog scene-assets/catalog.json --id carved-altar-v1 --name "Carved altar" \
  --category altar --license user-provided --source-url local:artist-library \
  --creator "Asset artist" --quality production --dimensions-m 2.0 0.8 1.1
```

Use a new ID when source bytes change. A catalogue entry includes name, semantic
category, tags, source, creator, license, digest, quality and optional dimensions.
Missing required models produce an actionable waiting state, preserving the
program/checkpoints. Add suitable models and resume; do not substitute unrelated
assets solely to finish a job. The example catalogue is a small validation set,
not a universal collection of scene-specific models.

Configure actual registered text and image models in the example config.
Custom entries in `~/.loop/agent/models.json` can declare a verified image model
as `{"id":"gemma-4-31b","input":["text","image"]}`. The loader now preserves
that metadata. A successful actual-image probe confirmed this endpoint's model;
the local entry was updated and its original file saved as
`~/.loop/agent/models.before-scene-vision-upgrade.json`. Do not label a text-only
model as image-capable to bypass preflight.
`quality.fidelity: realistic` requires visual review and rejects legacy proxy
recipes. `concept` allows deliberate concept work. New Python jobs default to
realistic; supplied configurations retain their explicit settings. For a cheap
concept run use `--fidelity concept --skip-visual-review` on either wrapper.
Never interpret that result as a photorealism validation.

`max_agent_tasks` bounds regional model calls; `max_blender_processes` bounds
component jobs. Start with 2–3 model calls and 1–2 Blender jobs, then adjust for
available memory and endpoint capacity. Node, token, wall-time, triangle and
texture limits remain explicit. A larger extent limit permits large coordinates;
it does not implement world streaming, automatic mesh LOD or unlimited detail.

Existing individual-run snapshots without the new planning key resume with the
legacy planner. Select `planning.mode: legacy` explicitly to reproduce old
recipe workflows. Starting a new adaptive batch does not rewrite old prompts,
briefs or accepted results.

## Inspect actual diversity and quality

```bash
python3 scripts/audit_scene_diversity.py scene-runs/collection-60 \
  --output scene-runs/collection-60/diversity-audit.json
cargo test -p loop-scene --lib --locked
python3 -m unittest discover -s tests -p 'test_*.py'
```

The audit compares generated recipe mix, normalized spatial occupancy, hierarchy,
authored geometry and asset identity rather than scene names. Similarity is a
review aid: several intentionally related rooms can legitimately share structure.
Image findings now include insufficient coverage and requests for another view;
actual semantic node membership is checked instead of merely counting IDs.
Per-attempt image review reports are retained under `validation/` even when a
scene is rejected. Review prompts retain intent, materials and placement while
omitting raw vertex arrays and local source paths, reducing context cost for
detailed geometry.

Visual repairs can redesign the affected adaptive region within its existing
allocation and required-feature contract. Each corrected plan is checked before
regeneration; unrelated regions remain intact. Changes to the parent topology
require a separate revision. Camera and environment corrections now use bounded
presentation revisions that preserve geometry and regenerate review views.
Unambiguous top-level
recipe fields are normalized into `parameters` locally, avoiding paid schema-only
retries while continuing to reject conflicts and unknown fields.
Numerical regional repairs can return only changed object records; omitted
objects retain their geometry and placement exactly. This avoids re-emitting
large, unaffected parts of a region on each correction.
The compiler also canonicalizes authored coordinates into tight local bounds
without moving their world-space geometry. Sweep bounds include the full cross
section. Hard regional containment remains enforced. Image-token reservations
scale with preview resolution and are reconciled against actual provider usage.

This removes major architectural restrictions and introduces a real-model path.
It cannot guarantee a perfect arbitrary scene. Results still depend on model
reasoning, appropriate available assets, authored materials, camera coverage and
review quality. Asset acquisition for every possible subject, terrain ecosystems,
geometric structural simulation, world streaming and unrestricted parent redesign
remain areas for further work. The pipeline must report these limits honestly
instead of marking a generic substitute as success.

## Full-detail offline production extension

The user's production target is the highest-quality offline world, with heavy
rendering acceptable on another system. Original source detail is now the default
policy. See [the production workflow](examples/adaptive/PRODUCTION.md) and
[offline configuration](examples/adaptive/production-offline.json).

Implemented additions include scene-authored physical sky and lights, separate
preview/delivery settings, strict optional acceptance of visual warnings, source
technical inspection during import, persistent missing-component requirements,
ranked local/provider search, exact source/material kinds and curator-approved
surface-use restrictions. Contact acceleration preserves the exhaustive sampled
vertex result. Optional measured LOD derivatives are implemented for separately
requested uses and are forbidden by the full-detail default.

Source inspection reports mesh topology, measured bounds, UV problems, texture
resolution/dependencies and connected material channels. These measurements
identify broken or incomplete assets; they do not turn a low-quality source into
an artist-quality model. Existing small validation assets are retained for
reproducibility, including their original resolutions. New acquisitions default
to 4k when available, with explicit 8k/16k selection supported.
