# How the pipeline works

The pipeline uses Loop for model calls and Blender to build, render, and export
3D scenes.

## Main flow

```text
Category counts → prompt planner + diversity critic ─┐
One prompt per line / explicit scene tasks ──────────┤
                                                    ↓
Preflight → scene program → structure review → concurrent regional design
          → Blender components → assembly and previews
          → validation / bounded repair → accepted exports → dataset and gallery
```

Preflight runs before generation: model/credential checks, endpoint reachability,
storage checks, and a small real Blender export/render test. Endpoint reachability
alone does not prove that inference credentials work.

1. **Schedule inputs.** [scene_batch.py](scene_batch.py) reads category counts,
   saves configuration and progress, and starts bounded CLI subprocesses.
   It plans up to five prompts at a time and runs ready scenes before planning
   more. [generate_scenes.py](generate_scenes.py) runs existing prompt lines
   sequentially; category inputs and batch resumes delegate to `scene_batch.py`.
2. **Invent and review prompts.**
   [prompt_planning.rs](crates/loop-scene/src/prompt_planning.rs) calls the planner
   and diversity critic. Local checks reject duplicates and malformed results;
   the critic compares concrete designs against prior concepts. Revisions retain
   feedback and checkpoints. An accepted prompt becomes a scene task.
3. **Design the scene program.**
   [adaptive.rs](crates/loop-scene/src/adaptive.rs) asks `SceneDirector` to derive
   topology, region envelopes, scale/detail allocations, essential features and
   inspection cameras from the request and source catalogue. `StructureDesigner`
   independently reviews it. Regions are invisible work allocations, not rooms.
4. **Design regional geometry.** Scoped designers run with bounded concurrency,
   producing imported asset references or explicit meshes, extrusions, revolved
   profiles, swept forms and terrain surfaces. Deterministic compilation checks
   bounds, ownership, feature allocation and shared region connections. Regional
   corrections and paid-work checkpoints are bounded and input-hashed. Missing
   essential models remain explicit blockers instead of placeholder geometry.
5. **Build and render.** The Rust Blender adapter starts headless Blender jobs.
   Component workers run within `max_blender_processes`. Exact repeated geometry
   reuses promoted components; repeated source models share mesh data in assembly.
   Original model materials and textures survive import. Generated components are
   isolated per scene; the verified source catalogue can be shared across scenes.
   Overview and authored regional cameras provide actual review images.
6. **Validate and publish.** Geometry, render health, and export checks gate
   acceptance. Blender reopens the native file and reimports the GLB for parity
   checks. Realistic fidelity requires image review and excludes legacy proxy
   recipes. Missing image coverage or requested additional views blocks acceptance.
   Repairable findings can trigger bounded repair/regeneration. Successful runs
   publish `final/`; the Python wrapper updates the gallery and `dataset.json`.

## Loop harness, agents, and API calls

There are two execution paths sharing Loop's model/provider infrastructure:

- **Coding agent:** CLI/desktop →
  [app bootstrap](crates/loop-app-core/src/runtime.rs) →
  [AgentHarness](crates/loop-agent/src/harness/agent_harness.rs).
  The harness manages conversation turns, sessions, tools, approvals, and sandbox
  environments. [loop-orchestration](crates/loop-orchestration/src/lib.rs) provides
  reusable task-graph scheduling infrastructure.
- **Scene generation:**
  [CLI scene commands](crates/loop-cli/src/scene.rs) load the shared credentials
  and model registry, then call `SceneRunController` or the prompt planner.
  [RoleInvoker](crates/loop-scene/src/roles.rs) calls
  `loop_ai::Models::stream_simple` directly with role instructions and typed JSON
  contracts. Scene roles have no coding tools and do not launch `AgentHarness`
  coding subagents. The controller owns sequencing and state.

“Agents” in the scene flow means specialized model roles: prompt planning,
independent diversity review, scene design, structure review, furnishing,
visual review, and repair. Text roles can share a model; review requires a model
registered for image input. Shared call/token reservations bound concurrent calls.
`--workers` controls concurrent scene processes, not autonomous subagents.

LLM/API calls occur in `roles.rs`: text calls produce or repair structured plans;
image calls review rendered views. Schema corrections and transient retries may
make additional calls. `loop-ai` handles provider transport and streaming;
model-catalog refresh and endpoint probes are separate HTTP requests. Geometry
construction, local fitting, rendering, and numerical validation run locally.

## Other flows

- With `planning.mode: legacy` and `models.compact_planning: false`, the controller uses recursive frontier
  proposals: the scene director allocates nodes, then scoped design roles expand
  owned subtrees. This converges on the same scene specification, backend,
  validation, and export stages as compact planning.
- With `planning.mode: legacy` and `models.compact_planning: true`, the original
  recipe-layout, automatic-room and sequential furnishing path remains available.
  Old individual-run snapshots without a planning key resume this legacy flow.
  New configurations default to adaptive planning regardless of compact_planning.
- `loop scene run --config CONFIG --tasks TASKS` accepts explicit task lists,
  bypassing category invention. See [examples/scenes.yaml](examples/scenes.yaml).
  `scene status`, `resume`, `inspect`, and `cancel` operate on individual runs.
- The optional [desktop app](crates/loop-desktop) uses GPUI and the shared coding
  harness. It needs nightly Rust and platform GUI libraries; it is not required
  for scene batches. On Linux these include fontconfig, XCB, xkbcommon and Wayland.
- [scene-production-20260918/run_batch.py](scene-production-20260918/run_batch.py)
  reproduces three fixed showcase interiors. Its builder was created using Loop
  model drafts followed by substantial supervised code repair and visual review.
  It makes no new model calls and is separate from generic prompt generation.
  Run `python3 scene-production-20260918/run_batch.py --help` for its options.

## Files to start with

| File/module | Responsibility |
|---|---|
| [scene_batch.py](scene_batch.py) | Durable category queue, process locks, retries, cleanup, progress totals |
| [generate_scenes.py](generate_scenes.py) | Prompt-file wrapper, accepted-only dataset index and HTML gallery |
| [crates/loop-cli/src/scene.rs](crates/loop-cli/src/scene.rs) | CLI entry points and shared model loading |
| [config.rs](crates/loop-scene/src/config.rs), [tasks.rs](crates/loop-scene/src/tasks.rs) | Strict configuration, task schemas, overrides and defaults |
| [controller.rs](crates/loop-scene/src/controller.rs), [roles.rs](crates/loop-scene/src/roles.rs) | Scene execution and model role contracts |
| [spec.rs](crates/loop-scene/src/spec.rs) | Scene/node contracts, transforms, bounds and interfaces |
| [backends/blender.rs](crates/loop-scene/src/backends/blender.rs) | Blender discovery, subprocess jobs and artifacts |
| [scene_backend.py](crates/loop-scene/backends/blender/scene_backend.py) | Procedural recipes, assembly, rendering and export |
| [validation.rs](crates/loop-scene/src/validation.rs), [recovery.rs](crates/loop-scene/src/recovery.rs) | Acceptance checks and failure/retry policy |
| [storage.rs](crates/loop-scene/src/storage.rs), [reporting.rs](crates/loop-scene/src/reporting.rs) | SQLite state, promotion, timings and provider-reported usage |
| [retrieval.rs](crates/loop-scene/src/retrieval.rs) | Scene-local asset indexing and compatible reuse |
| [adaptive.rs](crates/loop-scene/src/adaptive.rs) | Scene programs, scoped concurrent design and checkpoints |
| [asset_catalog.rs](crates/loop-scene/src/asset_catalog.rs), [scene_assets.py](scripts/scene_assets.py) | Verified source models, acquisition, variants and provenance |
| [audit_scene_diversity.py](scripts/audit_scene_diversity.py) | Realized structural similarity reports |
| [crates/loop-ai/src](crates/loop-ai/src) | Model registry, provider APIs and streaming |
| [tests](tests) | Python wrapper/scheduler and Blender checks; Rust tests also live beside modules |

## Checkpoints and dataset boundaries

Batch progress lives in `progress.json`; each scene has `run.db`, saved input
snapshots, designs, model responses, and reports. Resume reuses accepted work.
Geometry uses right-handed metres, Z-up, parent-local transforms and `xyzw`
quaternions; Blender's exporter performs the glTF basis conversion.

The category scheduler archives superseded attempts under `attempt-history/` and
includes their usage in totals. Archives retain diagnostics, but internal paths
still point at their original locations, so they are not supported resume targets.
Active/incomplete runs may remain under `scenes/`; use `dataset.json` for ingestion.
It selects one accepted run per task with nonempty manifest, Blend and GLB files.
Acceptance verifies configured checks, not photorealism or guaranteed prompt fidelity.
