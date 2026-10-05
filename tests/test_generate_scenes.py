import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

SPEC = importlib.util.spec_from_file_location(
    "generate_scenes", Path(__file__).resolve().parents[1] / "generate_scenes.py")
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class GenerateScenesTests(unittest.TestCase):
    def test_category_manifest_and_resume_route_to_durable_scheduler(self):
        with patch.dict(sys.modules, {"scene_batch": SimpleNamespace(main=lambda args: (args, 7))}):
            for args in (["categories.yaml", "--name", "collection"],
                         ["--output", "/tmp/out", "categories.yml"],
                         ["--resume", "/tmp/collection"]):
                self.assertEqual(module.main(args), (args, 7))

    def test_yaml_config_does_not_route_plain_prompt_input(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "prompts.txt").write_text("A gallery\n")
            (root / "config.yaml").write_text("version: 1\n")
            with patch.dict(sys.modules, {"scene_batch": SimpleNamespace(main=lambda args: self.fail("wrong dispatcher"))}):
                self.assertEqual(module.main([str(root / "prompts.txt"), "--config", str(root / "config.yaml"),
                                              "--output", str(root / "outputs"), "--dry-run"]), 0)

    def test_bom_blank_unicode_and_literal_shell_text(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "prompts.txt"
            path.write_text('\ufeff\n Café room\n\n$(touch nope) "desk"\n', encoding="utf-8")
            self.assertEqual(module.read_prompts(path), ['Café room', '$(touch nope) "desk"'])

    def test_empty_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "empty.txt"
            path.write_text("\n  \n")
            with self.assertRaises(ValueError):
                module.read_prompts(path)

    def test_refresh_reconciles_resumed_status_and_interior_gallery(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            run = root / "scenes/scene-001-test"
            final = run / "final"
            (final / "previews").mkdir(parents=True)
            for name in ("manifest.json", "scene.blend", "scene.glb", "previews/interior_room.png"):
                (final / name).write_text("fixture")
            metrics = {"status": "accepted", "active_minutes": 2.5, "wall_minutes": 4,
                       "models_used": ["test-model"], "reported_tokens": 123,
                       "interiors": {"room_count": 1, "furnished_count": 1}}
            module.write_json(run / "run-report.json", metrics)
            module.write_json(final / "run-report.json", metrics)
            module.write_json(root / "tasks.json", {"tasks": [{"id": "scene-001", "prompt": "Room"}]})
            module.write_json(root / "batch-report.json", {"results": [{"id": "scene-001", "status": "failed"}]})
            report = module.refresh_batch(root)
            self.assertTrue(report["complete"])
            self.assertTrue(report["results"][0]["accepted"])
            self.assertEqual(report["results"][0]["elapsed_minutes"], 2.5)
            self.assertIn("interior_room.png", (root / "index.html").read_text())
            self.assertIn("test-model", (root / "index.html").read_text())

    def test_live_log_preserves_output_and_exit_code(self):
        with tempfile.TemporaryDirectory() as folder:
            log = Path(folder) / "run.log"
            result = module.run_with_log([sys.executable, "-c", "print('progress'); raise SystemExit(2)"], log)
            self.assertEqual(result.returncode, 2)
            self.assertIn("progress", log.read_text())

    def test_gallery_escapes_prompt_and_links_outputs(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            final = root / "scenes/scene-001-test/final"
            final.mkdir(parents=True)
            for name in ("manifest.json", "scene.blend", "scene.glb"):
                (final / name).write_text("test fixture")
            (final.parent / "run-report.json").write_text(json.dumps({"status": "accepted"}))
            module.update_gallery(root, [{"id": "scene-001", "prompt": "<script>example</script>"}])
            page = (root / "index.html").read_text()
            self.assertNotIn("<script>", page)
            self.assertIn("scene-001-test/final/scene.glb", page)

    def test_dry_run_and_failure_continuation(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            prompts = root / "prompts.txt"
            prompts.write_text("Reading room\nDining room\nOffice\n")
            with patch.object(module.subprocess, "run") as run:
                self.assertEqual(module.main([str(prompts), "--output", str(root / "dry"),
                                               "--dry-run"]), 0)
                run.assert_not_called()
            config = next((root / "dry").glob("*/config.json"))
            self.assertFalse(json.loads(config.read_text())["quality"]["require_visual_review"])
            self.assertTrue(json.loads(config.read_text())["models"]["compact_planning"])
            with patch.object(module, "find_loop", return_value=Path("/fake/loop")), \
                    patch.object(module.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")), \
                    patch.object(module, "run_with_log", side_effect=[
                        subprocess.CompletedProcess([], code, stdout="", stderr="")
                        for code in (0, 1, 0)]) as run:
                self.assertEqual(module.main([str(prompts), "--output", str(root / "live")]), 1)
                self.assertEqual(run.call_count, 3)
                task = json.loads(Path(run.call_args.args[0][-1]).read_text())
                self.assertEqual(task["tasks"][0]["prompt"], "Office")
            report = json.loads(next((root / "live").glob("*/batch-report.json")).read_text())
            self.assertTrue(report["complete"])
            self.assertEqual([r["accepted"] for r in report["results"]], [True, False, True])

    def test_preflight_blocks_generation(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            prompts = root / "prompts.txt"
            prompts.write_text("Office\n")
            with patch.object(module, "find_loop", return_value=Path("/fake/loop")), \
                    patch.object(module.subprocess, "run", return_value=
                                 subprocess.CompletedProcess([], 1, "reviewer missing", "")) as run:
                self.assertEqual(module.main([str(prompts), "--output", str(root)]), 1)
                self.assertEqual(run.call_count, 1)
                self.assertEqual(run.call_args.args[0][2], "doctor")
            report = json.loads(next(root.glob("*/batch-report.json")).read_text())
            self.assertEqual(report["results"], [])
            self.assertIn("Preflight failed", report["blocker"])


if __name__ == "__main__":
    unittest.main()
