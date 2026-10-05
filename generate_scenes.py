#!/usr/bin/env python3
"""Generate scenes from prompts.txt or category-count YAML; resume named batches with --resume."""

import argparse
from datetime import datetime, timezone
import json
import html
import os
from pathlib import Path
import subprocess
import sys
import uuid
import time

ROOT = Path(__file__).resolve().parent


def read_prompts(path):
    prompts = [line.strip() for line in path.read_text(encoding="utf-8-sig").splitlines()
               if line.strip()]
    if not prompts:
        raise ValueError("The prompt file is empty. Add one prompt per line.")
    return prompts


def find_loop():
    if os.environ.get("LOOP_BIN"):
        binary = Path(os.environ["LOOP_BIN"]).expanduser().resolve()
    else:
        candidates = [ROOT / "target" / profile / "loop"
                      for profile in ("debug", "release")]
        candidates = [p for p in candidates if p.is_file()]
        if not candidates:
            raise ValueError("Build Loop first: cargo build -p loop-cli --locked")
        binary = max(candidates, key=lambda p: p.stat().st_mtime)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError(f"Loop executable is unavailable: {binary}")
    return binary


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def run_with_log(command, log_path):
    """Keep live progress visible and preserve all diagnostics for this prompt."""
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, text=True, bufsize=1)
        try:
            for line in process.stdout:
                print(line, end="", flush=True)
                log.write(line)
                log.flush()
            code = process.wait()
        except BaseException:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            raise
        finally:
            process.stdout.close()
    return subprocess.CompletedProcess(command, code)


def accepted_run(run):
    """Published datasets require controller acceptance and all native/export files."""
    try:
        report = json.loads((run / "run-report.json").read_text())
        return report.get("status") == "accepted" and all(
            (run / "final" / name).is_file() and (run / "final" / name).stat().st_size > 0
            for name in ("manifest.json", "scene.blend", "scene.glb"))
    except (OSError, ValueError, AttributeError):
        return False


def update_gallery(batch, tasks):
    sections = []
    dataset = []
    for task in tasks:
        finals = sorted(p for p in (batch / "scenes").glob(task["id"] + "-*/final/manifest.json")
                        if accepted_run(p.parent.parent))
        if not finals:
            continue
        folder = finals[-1].parent
        if not all((folder / name).is_file() for name in ("scene.blend", "scene.glb")):
            continue
        relative = folder.relative_to(batch).as_posix()
        dataset.append({"id": task["id"], "prompt": task["prompt"],
                        "manifest": f"{relative}/manifest.json",
                        "blend": f"{relative}/scene.blend", "glb": f"{relative}/scene.glb"})
        preview = folder / "previews/exterior_three_quarter.png"
        picture = f'<img src="{relative}/previews/exterior_three_quarter.png" alt="Scene preview">' if preview.exists() else ""
        report_path=folder / "run-report.json"
        metrics=json.loads(report_path.read_text()) if report_path.exists() else {}
        details=""
        if metrics:
            details=f'<p>Status: {html.escape(metrics.get("status","unknown"))} · {metrics.get("active_minutes",0):.2f} active minutes · Models: {html.escape(", ".join(metrics.get("models_used",[])))} · <a href="{relative}/run-report.md">Timing/status report</a></p>'
        interior_views=sorted((folder / "previews").glob("interior_*.png"))
        if interior_views:
            details+='<h3>Furnished interiors</h3>'+''.join(f'<a href="{relative}/previews/{p.name}"><img style="width:49%" src="{relative}/previews/{p.name}" alt="Furnished room"></a>' for p in interior_views)
        if (folder / "previews/cutaway.png").exists():
            details+=f'<p><a href="{relative}/previews/cutaway.png">Whole-building cutaway</a></p>'
        sections.append(f'<section><h2>{html.escape(task["id"])}</h2><p>{html.escape(task["prompt"])}</p>{picture}'
                        f'<p><a href="{relative}/scene.blend">Blender</a> · <a href="{relative}/scene.glb">GLB</a> · '
                        f'<a href="{relative}/report.md">Validation report</a></p>{details}</section>')
    page = '<!doctype html><meta charset="utf-8"><title>Generated scenes</title><style>body{max-width:1100px;margin:40px auto;background:#17202a;color:#eee;font:17px system-ui}img{width:100%}a{color:#8dd8ff}section{margin:40px 0}</style><h1>Generated scenes</h1><p>Procedural outputs. See each validation report for review status. Native volumetric beams use a translucent approximation in GLB.</p>'
    (batch / "index.html").write_text(page + "".join(sections), encoding="utf-8")
    write_json(batch / "dataset.json", {"version": 1, "scenes": dataset})


def refresh_batch(batch):
    """Reconcile reports/gallery after individual CLI resume commands."""
    tasks=json.loads((batch / "tasks.json").read_text())["tasks"]
    report=json.loads((batch / "batch-report.json").read_text())
    old={r["id"]:r for r in report.get("results",[])}
    results=[]
    for task in tasks:
        paths=sorted((batch / "scenes").glob(task["id"]+"-*/run-report.json"))
        if not paths:
            if task["id"] in old:results.append(old[task["id"]])
            continue
        value=json.loads(paths[-1].read_text());accepted=value["status"]=="accepted"
        results.append({"id":task["id"],"status":value["status"],"accepted":accepted,
                        "exit_code":0 if accepted else None,"elapsed_minutes":value["active_minutes"],
                        "wall_minutes":value["wall_minutes"],"models_used":value["models_used"],
                        "reported_tokens":value["reported_tokens"],"interiors":value.get("interiors"),"run_report":str(paths[-1])})
    report["results"]=results;report["complete"]=len(results)==len(tasks) and all(r.get("status")!="running" for r in results)
    write_json(batch / "batch-report.json",report);update_gallery(batch,tasks)
    return report


def main(argv=None):
    # Keep the original one-prompt-per-line interface while routing category
    # manifests and durable resumes to the large-scale scheduler. Parse known
    # options here so a YAML --config is not mistaken for the input manifest.
    arguments = list(sys.argv[1:] if argv is None else argv)
    probe = argparse.ArgumentParser(add_help=False)
    probe.add_argument("input", nargs="?")
    for flag in ("--config", "--briefs", "--output", "--name", "--resume", "--workers",
                 "--seed", "--max-attempts", "--retry-base", "--retry-cap",
                 "--retry-seconds", "--retry-max-seconds", "--max-redesigns"):
        probe.add_argument(flag)
    probe.add_argument("--dry-run", action="store_true")
    probe.add_argument("--plan-only", action="store_true")
    probe.add_argument("--retry-budget-blocked", action="store_true")
    probe.add_argument("--retry-auth-blocked", action="store_true")
    known, _ = probe.parse_known_args(arguments)
    if known.resume or (known.input and Path(known.input).suffix.lower() in {".yaml", ".yml"}):
        from scene_batch import main as batch_main
        return batch_main(arguments)
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        "Category workflow: python3 generate_scenes.py categories.yaml --name collection. "
        "Resume: python3 generate_scenes.py --resume scene-runs/collection. "
        "All category options: python3 scene_batch.py --help."))
    parser.add_argument("prompts", nargs="?", type=Path, help="Text file: one scene prompt per line")
    parser.add_argument("--refresh-batch",type=Path,help="Refresh a batch's gallery/status after CLI resumes; does not generate scenes")
    parser.add_argument("--output", type=Path, default=ROOT / "scene-runs",
                        help="Parent folder for new batches (default: scene-runs)")
    parser.add_argument("--config", type=Path,
                        help="Optional existing scene config, including reviewer role settings")
    parser.add_argument("--dry-run", action="store_true", help="Prepare inputs without model calls")
    parser.add_argument("--require-visual-review", action="store_true",
                        help="Require image-model review in addition to geometry/export checks")
    args = parser.parse_args(argv)
    try:
        if args.refresh_batch:
            if args.prompts:raise ValueError("Use either prompts or --refresh-batch, not both")
            refresh_batch(args.refresh_batch.resolve());print(f"Reports refreshed: {args.refresh_batch.resolve() / 'index.html'}");return 0
        if args.prompts is None:raise ValueError("Provide a prompt file or --refresh-batch")
        prompts = read_prompts(args.prompts)
        binary = None if args.dry_run else find_loop()
        config = args.config.expanduser().resolve() if args.config else None
        if config and not config.is_file():
            raise ValueError(f"Config does not exist: {config}")
        batch = args.output.expanduser().resolve() / (
            datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S") + "-" + uuid.uuid4().hex[:8])
        batch.mkdir(parents=True, exist_ok=False)
        if config is None:
            config = batch / "config.json"
            write_json(config, {
                "version": 1,
                "models": {"provider": "soket", "default_model": "gemma-4-31b",
                           "fallback_models": ["gpt", "qwen3-8-27b"],
                           "compact_planning": True},
                "backend": {"generator": "blender", "variation": "procedural_pbr_v1",
                            "preview_renderer": "cycles", "exports": ["blend", "glb"]},
                "quality": {"preview_resolution": [1600, 1200], "preview_samples": 64,
                            "require_visual_review": args.require_visual_review},
                "storage": {"output_root": str(batch / "scenes"),
                            "asset_library": str(args.output.expanduser().resolve() / "library"),
                            "min_free_bytes": 2000000000},
            })
        tasks = [{"id": f"scene-{i:03d}", "prompt": prompt, "enabled": True,
                  "overrides": {"storage": {"output_root": str(batch / "scenes")},
                                **({"quality": {"require_visual_review": True}}
                                   if args.require_visual_review else {})}}
                 for i, prompt in enumerate(prompts, 1)]
        write_json(batch / "tasks.json", {"version": 1, "tasks": tasks})
        print(f"{len(tasks)} scene(s). Output: {batch}", flush=True)
        if args.config is None and not args.require_visual_review:
            print("Generation mode: geometry/export checks enabled; automated visual review disabled.", flush=True)
        if args.dry_run:
            print("Inputs prepared; no generation or model calls performed.")
            return 0
        report = {"config": str(config), "results": [], "complete": False}
        write_json(batch / "batch-report.json", report)
        print("Checking model, reviewer, Blender, and storage prerequisites...", flush=True)
        preflight = subprocess.run([str(binary), "scene", "doctor", "--config", str(config)],
                                   cwd=ROOT, check=False, capture_output=True, text=True)
        (batch / "preflight.log").write_text(preflight.stdout + preflight.stderr, encoding="utf-8")
        if preflight.returncode != 0:
            print(preflight.stdout + preflight.stderr, flush=True)
            report["blocker"] = "Preflight failed; no scene generation started. See preflight.log."
            write_json(batch / "batch-report.json", report)
            print(report["blocker"], flush=True)
            return 1
        for i, task in enumerate(tasks, 1):
            print(f"\n[{i}/{len(tasks)}] {task['prompt']}", flush=True)
            task_file = batch / f"{task['id']}.json"
            write_json(task_file, {"version": 1, "tasks": [task]})
            started=time.monotonic()
            result = run_with_log([str(binary), "scene", "run", "--config", str(config),
                                   "--tasks", str(task_file)], batch / f"{task['id']}.log")
            minutes=(time.monotonic()-started)/60
            reports=sorted((batch / "scenes").glob(task["id"]+"-*/run-report.json"))
            metrics=json.loads(reports[-1].read_text()) if reports else {}
            report["results"].append({"id": task["id"], "exit_code": result.returncode,
                                      "accepted": result.returncode == 0,"elapsed_minutes":minutes,
                                      "status":metrics.get("status","accepted" if result.returncode==0 else "failed"),
                                      "models_used":metrics.get("models_used",[]),"run_report":str(reports[-1]) if reports else None,
                                      "reported_tokens":metrics.get("reported_tokens"),"interiors":metrics.get("interiors")})
            print(f"Scene {task['id']}: {report['results'][-1]['status']} in {minutes:.2f} minutes",flush=True)
            write_json(batch / "batch-report.json", report)
            update_gallery(batch, tasks)
        report["complete"] = True
        write_json(batch / "batch-report.json", report)
        accepted = sum(r["accepted"] for r in report["results"])
        print(f"\nFinished: {accepted}/{len(tasks)} accepted. Output: {batch}", flush=True)
        print(f"Gallery: {batch / 'index.html'}", flush=True)
        if accepted != len(tasks):
            print("Some runs failed or need review. See the run output and batch-report.json.")
            return 1
        return 0
    except (OSError, ValueError) as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\nInterrupted. Existing outputs are preserved.", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
