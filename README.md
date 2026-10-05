# 3D Scene Generation using Loop

This project generates 3D scenes using Loop for LLM planning and Blender for
geometry, rendering, and export.

It plans layouts and furnished rooms, exports `.blend` and `.glb` files, and
supports resumable batches with previews, progress reports, and an accepted-scene
dataset.
Outputs are procedural concept scenes; inspect the previews for visual quality.

## Setup

Use Linux or macOS for the Python batch runner (it uses POSIX process groups and
file locks). Install:

- A current stable Rust toolchain, Cargo, Git, and a native C/C++ build toolchain.
- Python 3.10+; the batch scripts need no third-party Python packages.
- Blender 4.5 (tested), available on `PATH` or through `BLENDER_PATH`.
- Access to a Soket-compatible model endpoint and a valid API key.

Run commands from the repository root:

```bash
cargo build -p loop-cli --locked
export SOKET_API_KEY="your-api-key"
export BLENDER_PATH="/absolute/path/to/blender"  # omit if Blender is on PATH
```

Using Loop’s existing credential handling, the pipeline also reads credentials from `~/.loop/agent/auth.json`. Custom provider
URLs and model registrations live in `~/.loop/agent/models.json`; scene model IDs
must exist in that catalog. The upstream Loop CLI (`target/debug/loop`) supports
`/login`, `/model`, and `/help`.

## Configure and run

Create `scene-config.json`. Replace the model ID if your endpoint uses another:

```json
{
  "version": 1,
  "models": {
    "provider": "soket",
    "default_model": "qwen3-8-27b",
    "fallback_models": [],
    "compact_planning": true
  },
  "backend": {"preview_renderer": "cycles"},
  "quality": {"require_visual_review": false}
}
```

This uses one model for all text roles. Numerical/export checks remain enabled.
Model requests and retries can incur charges. Keep several GB of disk space free.

### Optional visual validation

To review rendered images with a vision model, add this `roles` object inside
`models` in your configuration:

```json
"roles": {
  "visual_reviewer": {
    "model": "YOUR_VISION_MODEL_ID",
    "requires_images": true
  }
}
```

Set `quality.require_visual_review` to `true`. The reviewer model must be
registered in Loop's model catalog with image-input support; setting
`requires_images` does not make a text-only model accept images. The default
model continues to handle text planning.

Check the configuration before starting:

```bash
target/debug/loop scene doctor --config scene-config.json
```

For existing batches, update the saved batch configuration; existing scene runs
use their own `input/resolved-config.json` snapshots. Editing the original example
configuration does not update those snapshots.

### Start a batch

For a batch, create `categories.yaml` with category names and positive counts:

```yaml
museum: 2
forest sanctuary: 2
```

```bash
# Check credentials, model availability, Blender, and export/render support.
target/debug/loop scene doctor --config scene-config.json

# Plan prompts and generate scenes incrementally.
python3 scene_batch.py categories.yaml --name my-scenes --config scene-config.json

# Ctrl+C pauses safely. Resume the same batch:
python3 scene_batch.py --resume scene-runs/my-scenes
```

Use `--workers 2` for two concurrent scenes, `--dry-run` to prepare without model
calls, or `--plan-only` to generate prompts only. Defaults allow eight attempts
per scene/planning chunk and two fresh redesigns; limits persist across resumes.
An existing batch name requires `--resume`.

For existing prompts, put one complete prompt per line in `prompts.txt`:

```bash
python3 generate_scenes.py prompts.txt --config scene-config.json
```

Category batches require JSON configuration; the prompt-per-line wrapper also
accepts YAML. Example prompts are in [examples/interiors](examples/interiors)
and [examples/minecraft](examples/minecraft).

## Results and recovery

Under `scene-runs/<batch>/`:

- `progress.md` / `progress.json`: category-batch progress and errors.
- `index.html`: preview gallery. `dataset.json`: accepted exports for ingestion.
- `scenes/<run>/final/`: manifest, `.blend`, `.glb`, previews, and validation report.
- `logs/` and per-run `run-report.json`: failure details and reported model usage.
- `attempt-history/`: superseded category-batch attempts, excluded from the dataset.

Resume uses saved configuration. Existing scenes use
`input/resolved-config.json`, so changing the original config does not update them.
Fix the logged cause before raising `--max-attempts` or `--max-redesigns`.
Credential/configuration blockers require `--retry-auth-blocked` or
`--retry-config-blocked` after correction. `--retry-budget-blocked` retries a
budget-blocked scene after its saved budget is reviewed; it does not raise budgets.

```bash
# Inspect or resume one scene (pass its run folder, not the batch folder).
target/debug/loop scene status --run scene-runs/my-scenes/scenes/RUN_FOLDER
target/debug/loop scene resume --run scene-runs/my-scenes/scenes/RUN_FOLDER

# Clean an older category batch without generating anything; stop it first.
python3 scene_batch.py --resume scene-runs/my-scenes --cleanup
```

## Development

```bash
cargo test -p loop-scene --lib --locked
python3 -m unittest discover -s tests -p 'test_*.py'
```

See [PIPELINE.md](PIPELINE.md) for the execution flow, harness integration,
module map, and other entry points.
