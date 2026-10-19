# Scene pipeline upgrade checklist

Updated: 2026-10-19 (Asia/Kolkata). This is the working task ledger.
Requirements, architecture and usage: [UPGRADE.md](UPGRADE.md).

`[x]` means implemented and checked at the stated level. `[ ]` means pending;
implementation alone does not establish end-to-end visual quality.

## Audit and requirements

- [x] Read the pipeline, Loop role invocation, controller, schemas, Blender backend,
  wrappers, retrieval, validation and historical jobs.
- [x] Record the user's requirements: subject-specific topology and assets,
  broad scale, realistic morphology/materials, credible construction, diversity,
  efficient execution and honest quality gates. See `UPGRADE.md`.
- [x] Identify restrictive recipes, automatic room/furniture templates, terrain
  aliasing, primitive fallback, inactive concurrency limits and absent real models.
- [x] Identify latest batch's explicit single-rectangular-room brief; preserve
  saved jobs and provide a separate unrestricted path.
- [x] Audit 118 historical scene specifications. No exact structural duplicates;
  shared recipe vocabulary and spatial templates are the relevant problem.
  Evidence: `scene-runs/upgrade-validation/baseline-diversity.json` and each
  historical batch's `structural-diversity-audit.json`.

## Planning and execution

- [x] Add adaptive scene program with prompt-derived topology, region ownership,
  feature allocation, explicit asset gaps and configurable scale/detail budgets.
- [x] Add independent program review and bounded regional geometry corrections.
- [x] Add scoped region redesign from visual findings while preserving accepted
  parent bounds, topology and required features; checkpoint and validate repairs.
- [x] Normalize unambiguous recipe fields into `parameters` without another model
  call; conflicting values and unknown fields still fail. Regression tested.
- [x] Use object-level regional patches for numerical repairs; preserve omitted
  objects exactly and reject conflicting/removal IDs. Full plans remain accepted
  for compatibility. Regression tested.
- [x] Canonicalize authored geometry into tight local bounds while preserving
  world-space vertices; include sweep cross-sections in measured bounds. Apply
  the same normalization safely on resume. Geometry invariance tests pass.
- [x] Aggregate missing feature/geometry diagnostics and honor configured
  regional repair limits rather than reprinting a full plan for each error.
- [x] Reserve image tokens using preview resolution with a conservative fallback
  for unknown formats; reconcile against provider usage. Save raw image-review
  responses before schema parsing for durable diagnostics.
- [x] Add bounded concurrent regional specialists with input-hashed checkpoints.
- [x] Integrate adaptive planner with controller; retain explicit legacy mode and
  select legacy when resuming old snapshots without the new planning key.
- [x] Add bounded concurrent component generation, exact geometry reuse, and
  shared model-call/token reservations. Code integrated; final full tests below.
- [x] Add explicit region-local inspection cameras instead of assuming every
  scene can be judged through exterior or rectangular-room views.
- [x] Resolve configuration paths to absolute paths so task overlays do not
  accidentally rebase catalogue and output locations.
- [x] Review integration, resume, cache identity and concurrent budget behavior.
  Fixed missing task overrides, stale promoted geometry reuse, repair-source
  validation, immutable snapshot preservation and finite measured bounds.
  Review was completed by the primary agent after the delegated sessions ended.
- [x] Resolve explicit terrain/slab support references, including rotated support
  coordinates and tilted object bounds; test interpolation against the backend's
  actual heightfield triangles. Reject missing/cyclic/outside support references.
- [x] Extend support to lathe surfaces and refine contact against actual imported
  mesh vertices in Blender; persist measured placement. Actual Blender contact
  regression passes for tilted irregular meshes, rotated frames and idempotence.
- [x] Validate authored geometry before launching Blender and return measured
  rotated bounds in regional repair diagnostics. Document Z-up Euler rotation.
- [x] Bound active execution time across resumes and preserve per-attempt visual
  reports. Keep raw mesh coordinates out of image-review model context.

## Geometry and real assets

- [x] Add bounded declarative mesh, polygon extrusion, lathe, sweep and heightfield
  geometry; reject unsupported recipes instead of silently emitting a box.
- [x] Add analytic `path_arc` sweeps with bounded sampling, so smooth curves do
  not require the model to emit long vertex lists; preserve explicit paths too.
- [x] Keep review cameras above measured terrain by default, with explicit
  `allow_subsurface` for intentional underground views. Actual Blender regression
  verifies clearance and the opt-out. Camera metadata records any adjustment.
- [x] Add strict source catalogue with exact IDs, SHA-256, license, creator,
  quality declaration, optional dimensions and selected object/subtree.
- [x] Import GLB/packed Blender models with materials/textures intact, uniform
  scaling, bottom alignment, provenance and linked repeated geometry.
- [x] Add explicit local registration and bounded Poly Haven acquisition utility;
  losslessly pack glTF buffers and PBR images into self-contained GLB.
- [x] Download real pine sapling, rock and Chinese armchair validation assets.
  Local inventory: `scene-assets/catalog.json` (ignored generated content).
- [x] Add verified PBR surface-material reuse for authored geometry, metric UV
  tiling, and bounded texture acquisition. Download `forest_ground_04` as a
  source material. Blender tests verify texture retention, reuse and exclusion
  of source swatch geometry from the scene.
- [x] Finish single-tree variant catalogue setup and reproducible `variant`
  command. Selected `pine_sapling_small_a` has measured source dimensions.
- [x] Verify the selected single-tree variant through the complete live controller
  run, including assembly, native/GLB parity and actual image review.
- [x] Run actual Blender geometry/asset regression covering both source formats,
  bounds, textures, hashes, missing resources, linked instances and provenance.
- [x] Render real tree/rock/terrain study with authored detail camera.
  Evidence: `scene-runs/backend-quality-validation/`.
- [x] Inspect final accepted renders and export parity; regression-check the
  subsequent deterministic camera-clearance and analytic-arc additions.

## Diversity and acceptance

- [x] Remove universal room/furniture assumptions from prompt planning; retain
  semantic duplicate review without rejecting legitimate shared layout families.
- [x] Add realistic fidelity gate for unsupported proxies/unverified models.
- [x] Reject insufficient image coverage, extra-view requests and malformed
  reviewer evidence; check actual semantic node IDs, not only their count.
- [x] Default new wrapper jobs to realistic fidelity and image review; preserve
  explicit configs and saved resume settings; expose deliberate concept mode.
- [x] Add generated-structure diversity report with metadata-independent
  fingerprints and advisory nearest-scene comparisons.
- [x] Verify actual image capability: `gemma-4-31b` described the rendered pine;
  provider usage reported image tokens. Evidence: `vision-probe.json`.
- [x] Fix custom model loader dropping modality metadata and support documented
  `visual_reviewer` role key plus legacy `reviewer` alias. Back up and update the
  local model entry only after the successful image probe. Preflight passes.
- [x] Complete both end-to-end examples with actual model planning, generation,
  rendering and image review. Both accepted with warnings; details below.

## Tests, documentation and handoff

- [x] Asset packing/registration tests: 4 passed, including variant checksum failure.
- [x] Wrapper/batch/diversity tests: 55 passed in the completed focused run.
- [x] Targeted Rust validation/prompt tests: 14 passed.
- [x] CLI built successfully before the most recent integration/path changes.
- [x] Complete Rust scene library suite: 79 passed; CLI build passed. Logs:
  `scene-runs/upgrade-validation/rust-tests-final.log`, `build-final.log`.
- [x] Complete Python suite: 59 passed after variant-helper additions.
  Evidence: `scene-runs/upgrade-validation/python-tests-final.log`.
- [x] Blender asset/material, camera and mesh-contact regressions passed. Native
  component and assembly export/render integration tests: 2 passed.
- [x] Repeat assembly/export checks after measured-contact integration: both
  Blender integration tests pass. Corrected tree/rock/PBR study export parity
  passes with zero measured bounds residual; see its `parity/parity.json`.
- [x] Custom model-modality regression: 1 passed.
- [x] Accepted-run resume preserves the manifest hash and makes zero additional
  model calls. Evidence: `scene-runs/upgrade-validation/accepted-resume-check.json`.
- [x] Write requirements/architecture guide and adaptive config/task examples.
- [x] Update `README.md` and `PIPELINE.md` for new defaults, migration and limits.
- [x] Record final commands, outputs, live-run outcomes and remaining limitations
  in this checklist; review diff and leave a concise user handoff.

## Explicit limits and future work

These are not claims of completed capability:

- [ ] Universal production asset coverage and automatic acquisition for every
  domain. Current catalogue is intentionally small; missing required models block.
- [ ] Broad cross-category visual benchmark covering prayer rooms, collections,
  settlements, game worlds and large environments. Two small live examples are
  integration evidence, not evidence of universal scene quality.
- [ ] Resolve the remaining visual warnings in the example scenes: subtle terrain
  relief in the pine study and coarse material selection in the exhibition.
- [ ] Automatic mesh LOD, streamed world chunks and ecosystem-scale scattering.
- [ ] Engineering simulation and comprehensive geometric support/collision proofs.
- [ ] Unrestricted parent/topology/camera redesign after any visual rejection.
  Bounded redesign of an existing region is implemented.
- [x] Scene-authored physical sky, environmental lighting and exposure.
- [ ] Curated HDRI libraries and their source-resolution/provenance workflow.
- [ ] Guaranteed photorealism or perfect arbitrary scenes. Asset suitability,
  model reasoning, materials and actual image review still determine quality.

## Completed live validation

Both examples used real source assets, 640×480 Cycles previews at 8 samples,
Gemma for planning/image review, numerical validation and native/GLB parity.
Acceptance means no error/blocker findings; warnings remain in each manifest.

| Scene | Outcome | Evidence |
| --- | --- | --- |
| Pine regeneration plot | Accepted after scoped visual repair; ridge relief remains too subtle | [Manifest](scene-runs/upgrade-validation/live-scenes/pine-outcrop-01a14fb5-7c27-76d3-b9ca-91cb7d2aa3e0/final/manifest.json), [close view](scene-runs/upgrade-validation/live-scenes/pine-outcrop-01a14fb5-7c27-76d3-b9ca-91cb7d2aa3e0/final/previews/review_region-regeneration_plot_foliage_eye_level.png) |
| Chinese joinery exhibition | Accepted after replacing angular screen geometry; coarse architectural material warning remains | [Manifest](scene-runs/upgrade-validation/live-scenes/chair-exhibition-01a14df1-21fa-76b0-8027-9c98a0e96447/final/manifest.json), [whole view](scene-runs/upgrade-validation/live-scenes/chair-exhibition-01a14df1-21fa-76b0-8027-9c98a0e96447/final/previews/review_region-installation-core_cam-whole-installation.png) |

The pine run used 8 model calls, 68,103 reported tokens and 6.59 active minutes.
Its first numerical repair emitted only 699 bytes in 8 seconds, preserving the
rest of the plan. Eight geometry objects required four component jobs; repeated
saplings reused source geometry. The exhibition accumulated 20 calls, 124,257
tokens and 7.25 active minutes across development/resumes, so its timing is not
a clean-run benchmark. All paid attempts are retained in run reports.

Earlier Qwen/Gemma attempts exposed feature-reference, schema, local-coordinate,
contact and material problems; their failures remain recorded. GPT-OSS was also
tested, but was slower here and still failed placement constraints; no claim of
superior planner performance is made. A stopped GPT-OSS request was resumed from
its checkpoints rather than counted as a completed response.

The two accepted specifications have no repeated structural fingerprint:
`scene-runs/upgrade-validation/accepted-diversity.json`. This is a small-sample
structural check, not a broad visual-diversity benchmark.

The controlled corrected backend study remains separately labeled at
`scene-runs/upgrade-validation/contact-material-study/`. It confirms mesh contact,
PBR surface transfer and zero-residual export parity without pretending to be an
independently model-planned acceptance run.

## Verification commands

Run from the repository root. Logs are under `scene-runs/upgrade-validation/`.

```bash
cargo test -p loop-scene --lib --locked
cargo test -p loop-app-core custom_model_tests --locked
cargo test -p loop-scene --test blender_e2e --locked -- --ignored
cargo build -p loop-cli --locked
python3 -m unittest discover -s tests
git diff --check
```

Actual Blender scripts: `tests/blender_asset_geometry_check.py`,
`tests/blender_camera_check.py`, and `tests/blender_contact_check.py`; run with
`--background --factory-startup --disable-autoexec --python-exit-code 31 --python`.

## Next work

1. Recover `collection-60-1` with actionable geometry diagnostics and production settings.
2. Finish landscape visual corrections; preserve rejected results and source gaps.
3. Expand exact domain-specific sources and cross-category visual validation.
4. Implement streamed world chunks and richer terrain/surface authoring before claiming huge-world readiness.

## Production workflow extension (active)

The latest request extends the work toward reusable production components across
domains. These tasks remain separate from any claim of universal perfection.

- [x] Replace shared adaptive-scene lighting with a validated, scene-authored
  environment and purposeful lights; retain compatibility for historical runs.
- [x] Add explicit source kinds and curator-defined surface-use restrictions so
  available outdoor dirt cannot silently become an architectural finish.
- [x] Add ranked local/provider discovery and persistent asset requirements with
  explicit binding, provenance and unresolved gaps (asset-coverage agent).
- [x] Inspect actual source meshes, textures, UVs and dependencies; distinguish
  technical invalidity from artistic suitability (asset-quality agent).
- [x] Add opt-in, measured and cached LOD derivatives with source preservation,
  material/UV checks and explicit error limits (scale-components agent).
- [x] Honor clarified offline target: full-source-detail default, separate preview
  and native final render settings, and optional strict visual-warning policy.
  Document native Blender versus glTF lighting limitations. A real-time profile
  is outside the clarified target; optional derivatives stay disabled.
- [x] Accelerate contact placement by exact support-height pruning; regression
  agrees with exhaustive rays while skipping over 9,000 / 10,001 fixture vertices.
- [x] Add bounded camera/environment repair without regenerating geometry; keep
  corrected presentation metadata through later geometry repairs.
- [ ] Render and inspect new previews, run regression tests, record outcomes and
  preserve the remaining broad-domain/world-streaming work as pending.

Production-extension validation:

- [x] Rust scene suite: 82 passed; CLI build passed. Python suite: 75 passed.
  Logs: `production-rust.log`, `production-build.log`, `production-python.log`.
- [x] Actual Blender asset geometry, source QA, optional LOD, exact contact,
  cameras and scene presentation checks passed. QA/LOD fixtures were corrected
  to populate generated image buffers before packing and include node revision.
- [x] Both Blender native/GLB integration tests passed; controlled presentation
  comparison export parity passed with zero bounds residual.
- [x] Reopened the native comparison scene and verified saved 3840×2160,
  1024-sample Cycles settings independently of its 960×720 review renders.
- [x] Inspected real source pine and chair. Pine: 142,506 triangles / six textures;
  chair: 2,675 triangles / three textures. No technical errors; open-surface
  warnings remain appropriate for visual review. Reports: `source-qa/`.
- [x] Downloaded 2k clay plaster and concrete floor material studies; restricted
  their intended uses and forest soil uses in the local catalogue. Source models
  retain their original mesh and texture detail; acquisition defaults now use 4k.
- [x] Rendered controlled lighting/material comparison:
  [whole installation](scene-runs/upgrade-validation/presentation-study/review_region-installation-core_cam-whole-installation.png).
  This is a backend comparison using an existing accepted layout, not a fresh
  autonomous world-generation success.
- [ ] Finish independent model-planned presentation validation. The exhibition
  exhausted its 32-call budget without passing visual review; overexposure and
  material presentation remain unresolved. Its rejected renders and reports are
  retained, including `production-material-recovery.log`.

## Complexity and recovery follow-through (active)

- [x] Fix structural recipes silently ignoring source surface finishes. Blender
  regression covers slabs, walls, stairs, doors and windows, including separate
  glass preservation, source textures and metric UVs.
- [x] Include worker-source fingerprints in component cache identity so fixes
  invalidate old generated components on an incomplete run's resume.
- [x] Include configured source materials and environment in visual-review
  context; distinguish fallback colors from actual source shaders.
- [x] Preserve omitted cameras during presentation patches and merge partial
  environment fields; validate before applying. Detect unchanged/invalid repair
  proposals before rerendering and preserve paid proposals on disk.
- [x] Add deterministic full-detail asset distributions over arbitrary simple
  polygons with exclusions, seeded yaw/uniform scale, spatially indexed spacing,
  bounded placement attempts, explicit support and exact node-budget accounting.
  No proxy replacement or automatic density reduction is performed.
- [x] Verify deterministic placement, footprint/path clearance, impossible
  density rejection and expansion into real source nodes. Rust scene suite:
  85 passed; see `production-rust.log`.
- [x] Register the real source's B/C pine variants with measured bounds; three
  distinct source meshes are available for the contextual landscape study.
- [ ] Complete the 48-sapling / three-variant live landscape run, inspect actual
  ground/context/canopy views, and record measured geometry reuse and timings.
- [ ] Complete remaining autonomous exhibition corrections; do not treat its
  current rejected renders as accepted production output.

All log paths above are relative to `scene-runs/upgrade-validation/`.
The full-detail production profile and operational instructions are in
`examples/adaptive/production-offline.json` and `examples/adaptive/PRODUCTION.md`.


## Collection-60-1 recovery (active)

- [x] Audit all six planned scenes and historical attempts: 0/6 accepted,
  eight attempts per scene, 75 reported model attempts / 555,074 tokens at audit.
  Main failures: malformed or noncoincident region connections, generic lighting
  errors, missing sources, and repeated retries against unchanged checkpoints.
  Original config omitted a catalogue and disabled image review (concept defaults).
  Evidence: `scene-runs/collection-60-1/recovery/baseline-audit.json`.
- [x] Preserve original batch config/progress and every historical run. Add
  explicit `--recover-config` migration which creates a versioned batch config
  and fresh attempts without rewriting immutable run inputs or resetting spending
  history/lifetime attempts. Relative catalogue/backend paths remain valid.
- [x] Report all invalid region connections with exact IDs, local bounds and
  world coordinates. Support a single shared `world_at` point; reject conflicting
  coordinate representations, missing targets, self-links and out-of-bounds points.
- [x] Clarify authored architecture/terrain versus missing external source models
  and materials. Fix the example's invalid literal `models or textures` value.
- [x] Stop missing-source retries until explicitly requeued; recognize reservation
  budget failures as spending limits instead of redesign candidates.
- [x] Persist request-hashed paid raw responses before schema validation; recover
  them after parser fixes without extra model calls. Unbound historical response
  logs are not silently reused as trustworthy checkpoints.
- [x] Compact visual-repair input to authoring objects; remove repeated runtime
  provenance/cache paths. Support changed-object visual patches preserving omitted
  objects, exact transforms, source IDs, materials and support references.
- [x] Verify recovery code: 94 Rust scene tests and 78 Python tests passed; CLI built.
  Production recovery preflight passed actual Blender native/GLB export/render and
  the configured image-capable model checks.
- [x] Prepare strict production recovery config with real-source catalogue,
  full source detail, image review and warning-free acceptance. The explicitly
  requested Minecraft/voxel task retains intentional voxel geometry through a
  task override while keeping strict image review. Other tasks remain realistic.
- [ ] Complete the new recovery attempts, inspect their outputs and classify any
  remaining source gaps. Scheduler log: `scene-runs/collection-60-1/recovery/scheduler.log`.
- [ ] Expand missing exact domain components as identified; do not substitute
  generic models to make the batch show accepted.

### Resume diagnostics and fixes, 2026-10-19

- [x] Trace repeated dependency-review schema errors to recipe objects returned
  where tool-name strings were required. Provide explicit examples and preserve
  exact nested field paths through both normalized and ordinary JSON responses.
- [x] Add bounded image-review schema correction retaining actual images;
  checkpoint by images, requirements and model context, without defaulting missing
  coverage to success or charging replay tokens again.
- [x] Stop batch retries when the internal schema correction is exhausted;
  explicit `--retry-schema-blocked` allows recovery after a concrete fix.
- [x] Distinguish authored custom machinery from genuinely missing source models.
  The sci-fi run passed dependency review and reached regional geometry design.
- [x] Preserve spatial-program inventory context when a catalogue expands;
  resolve newly available sources through exact-ID dependency binding, with
  owner-region usage checks, rather than requiring a complete new design.
- [x] Acquire 4K aggregate concrete and weathered timber from Poly Haven; inspect
  both in Blender. Each has three packed 4096×4096 maps, no technical errors and
  one inspection warning. Source inspection is not scene-quality acceptance.
- [ ] Complete the pavilion recovery using the inspected timber source.
- [ ] Complete sci-fi region geometry, assembly and actual-image validation.
- [ ] Resolve remaining exact source gaps for the temple, fossil museum, voxel
  materials and karst landscape; no proxy or unrelated substitution is accepted.
- [ ] Deliver accepted previews and final manifests for all six batch tasks.

Current evidence is in `scene-runs/collection-60-1/recovery/`: `schema-recovery.log`,
`resume-rust-tests.log`, `resume-python-tests.log`, `aggregate-qa.json`, and
`timber-qa.json`. At this checkpoint the batch still has **0/6 accepted**. Planning
progress and passing software tests do not mean the six requested worlds are done.

Landscape follow-through: the three-variant run produced 48 full-detail saplings,
53 mesh objects sharing six mesh datablocks, 6,407,591 evaluated triangles and
20,135,823 packed texture bytes in assembly attempt 5. Native/GLB validation passed.
Image review correctly rejected floating corridor polygons, insufficient terrain
context and an occluded detail view. It then hit token reservation capacity with
an oversized repair prompt. The compact-patch recovery is now being exercised;
these previews are not accepted production results.

Shared distribution spacing now applies across different source prototypes,
including identical seeds; each pair respects the larger requested spacing.
Regression tests cover this along with schema normalization of exact recipe
containers, conflict rejection, and source-preserving compact repair transforms.
