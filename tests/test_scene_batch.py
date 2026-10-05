import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import subprocess
import sys
import textwrap
import types
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('scene_batch', Path(__file__).resolve().parents[1] / 'scene_batch.py')
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class Process:
    pid = 999999

    def __init__(self, code=0):
        self.returncode = code

    def poll(self):
        return self.returncode


class BatchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.input = self.root / 'categories.yaml'
        self.input.write_text('museum: 2\nlibrary: 1\n')
        self.args = types.SimpleNamespace(resume=None, categories=self.input, name='test-collection',
                                         config=None, output=self.root, seed=7, workers=2, max_attempts=4,
                                         max_redesigns=0, retry_seconds=.001, retry_max_seconds=.002)
        self.batch = module.Batch(module.prepare(self.args), self.args, Path('/fake/loop'))

    def tearDown(self):
        if self.batch.lock:
            self.batch.lock.close()
        self.temp.cleanup()

    def plan(self):
        def launch(key, command, log):
            request = module.read_json(Path(command[command.index('--request')+1]))
            rows = [{'name': f"{request['category']} concept {i}",
                     'prompt': f"{request['category']} unique scene {i}",
                     'diversity': {'topology': f'spiral-{i}'}}
                    for i in range(request['offset'], request['offset']+request['count'])]
            output = Path(command[command.index('--output')+1])
            module.atomic_json(output, {'prompts': rows})
            self.batch.live[key] = (Process(), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch):
            self.assertTrue(self.batch.plan())

    def test_configuration_failure_blocks_across_resumes_without_redesign(self):
        self.plan()
        self.batch.state['tasks'] = self.batch.state['tasks'][:1]
        task = self.batch.state['tasks'][0]
        self.args.max_attempts = None
        def launch(key, command, log):
            log.write_text('configured Soket model not found for scene_director: missing; one or more scenes are incomplete')
            self.make_run(task)
            self.batch.live[key] = (Process(1), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch) as calls, patch.object(module.time, 'sleep'):
            self.assertFalse(self.batch.generate())
            self.assertEqual(calls.call_count, 1)
        self.assertEqual(task['blocked_by'], 'configuration')
        self.assertEqual(task['redesigns'], 0)
        with patch.object(self.batch, 'launch') as calls:
            self.assertFalse(self.batch.generate())
            calls.assert_not_called()

    def test_generic_incomplete_summary_is_not_a_geometry_failure(self):
        self.assertEqual(module.failure_kind('one or more scenes are incomplete'), 'unknown')
        self.assertEqual(module.failure_kind('connection refused; one or more scenes are incomplete'), 'transient')
        self.assertEqual(module.failure_kind('HTTP 400: bad request; one or more scenes are incomplete'), 'configuration')

    def test_redesign_limit_survives_resume(self):
        self.plan()
        self.batch.state['tasks'] = self.batch.state['tasks'][:1]
        self.args.max_redesigns = 2
        self.batch.state['tasks'][0].update(status='needs_attention', blocked_by='redesign_limit', redesigns=2)
        with patch.object(self.batch, 'launch') as calls:
            self.assertFalse(self.batch.generate())
            calls.assert_not_called()

    def test_default_retry_limits_persist_and_zero_is_explicit(self):
        args = [str(self.input), '--output', str(self.root), '--name', 'bounded', '--dry-run']
        self.assertEqual(module.main(args), 0)
        root = self.root / 'bounded'
        self.assertEqual(module.read_json(root / 'progress.json')['retry_limits'], {'max_attempts': 8, 'max_redesigns': 2})
        self.assertEqual(module.main(['--resume', str(root), '--max-attempts', '0', '--dry-run']), 0)
        self.assertEqual(module.main(['--resume', str(root), '--dry-run']), 0)
        self.assertEqual(module.read_json(root / 'progress.json')['retry_limits']['max_attempts'], 0)

    def test_planner_configuration_failure_does_not_repeat_on_next_heartbeat(self):
        self.batch.state['categories'] = {'museum': 2}
        self.args.max_attempts = None
        def launch(key, command, log):
            log.write_text('configured Soket model not found for prompt_planner: missing')
            self.batch.live[key] = (Process(1), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch) as calls:
            self.batch.plan(one_chunk=True)
            self.batch.plan(one_chunk=True)
            self.assertEqual(calls.call_count, 1)
        self.assertEqual(self.batch.state['planning']['category-001-00000']['blocked_by'], 'configuration')

    def test_doctor_configuration_failure_stops_without_repeated_blender_probes(self):
        def launch(key, command, log):
            log.write_text('configured Soket model not found for default_model')
            self.batch.live[key] = (Process(1), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch) as calls:
            self.assertFalse(self.batch.doctor())
            self.assertEqual(calls.call_count, 1)
        self.assertEqual(self.batch.state['status'], 'needs_attention')

    def test_flat_yaml_and_reject_ambiguous_input(self):
        self.input.write_text('\ufeff# comment\n"stone: museums": 10\n\'winter villages\': 2 # count\n')
        self.assertEqual(module.categories_file(self.input), {'stone: museums': 10, 'winter villages': 2})
        for value in ('museum: 0', 'museum: -1', 'museum: 1.5', 'museum: 2\nmuseum: 3', 'a:\n  b: 2', 'x: true', 'x: 2\n---'):
            self.input.write_text(value)
            with self.assertRaises(ValueError):
                module.categories_file(self.input)

    def test_incremental_ready_scene_runs_before_missing_prompt_chunks(self):
        self.plan()
        self.batch.state['tasks'] = self.batch.state['tasks'][:1]
        first = self.batch.state['tasks'][0]
        # Match the surviving category count, leaving another category unplanned.
        self.batch.state['categories'] = {'museum': 1, 'library': 1}
        events = []
        def launch(key, command, log):
            events.append(command[2])
            task = next(t for t in self.batch.state['tasks'] if t['id'] == key)
            self.make_run(task, accepted=True)
            self.batch.live[key] = (Process(), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch), patch.object(module.time, 'sleep'):
            self.assertTrue(self.batch.generate(incremental=True))
        self.assertEqual(events, ['run', 'run'])
        self.assertEqual(first['status'], 'accepted')
        self.assertEqual(self.batch.state['totals']['accepted'], 2)

    def test_failed_chunk_moves_to_next_category_and_retains_feedback(self):
        requests = []
        def launch(key, command, log):
            request = module.read_json(Path(command[command.index('--request') + 1]))
            requests.append(request)
            output = Path(command[command.index('--output') + 1])
            if request['category'] == 'museum':
                log.write_text('Error: diversity critic rejected repeated radial layouts')
                draft = output.parent / 'response-planning'
                draft.mkdir()
                module.atomic_json(draft / 'draft.json', {'candidates': {'prompts': []}, 'rejection_feedback': ['avoid radial layouts']})
                process = Process(1)
            else:
                module.atomic_json(output, {'prompts': [{'name': 'woodland', 'prompt': 'unique woodland library', 'diversity': {}}]})
                process = Process()
            self.batch.live[key] = (process, io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch):
            self.assertIsNone(self.batch.plan(one_chunk=True))
            self.assertTrue(self.batch.plan(one_chunk=True))
            checkpoint = self.batch.state['planning']['category-001-00000']
            checkpoint['next_retry_epoch'] = 0
            self.assertIsNone(self.batch.plan(one_chunk=True))
        self.assertEqual([r['category'] for r in requests], ['museum', 'library', 'museum'])
        self.assertNotEqual(requests[0]['seed'], requests[1]['seed'])
        self.assertEqual(requests[2]['retry_index'], 1)
        self.assertEqual(requests[2]['rejection_feedback'], ['avoid radial layouts'])
        self.assertIn('diversity critic', checkpoint['error'])

    def test_outage_circuit_only_checks_doctor_until_recovery(self):
        calls = []
        def launch(key, command, log):
            calls.append(command[2])
            log.write_text('model_endpoint: connection refused')
            self.batch.live[key] = (Process(1 if len(calls) == 1 else 0), io.StringIO(), 0)
        snapshots = []
        with patch.object(self.batch, 'launch', side_effect=launch), patch.object(self.batch, 'wait_retry', side_effect=lambda seconds: snapshots.append(dict(self.batch.state))):
            self.assertTrue(self.batch.doctor())
        self.assertEqual(calls, ['doctor', 'doctor'])
        self.assertEqual(snapshots[0]['status'], 'waiting_endpoint')
        self.assertIn('next_check_epoch', snapshots[0])
        self.assertEqual(self.batch.state['planning'], {})
        self.assertNotIn('next_check_epoch', self.batch.state)

    def test_auth_rejection_requires_explicit_one_time_resume(self):
        self.batch.state['auth_blocked'] = True
        with patch.object(self.batch, 'launch') as launch:
            self.assertFalse(self.batch.doctor())
            launch.assert_not_called()
        self.assertIn('--retry-auth-blocked', self.batch.state['error'])

    def test_recovers_critique_before_later_transport_only_attempts(self):
        self.args.max_attempts = 40
        folder = self.batch.path / 'planning/category-001-00000'
        old = folder / 'attempt-0030/response-planning'
        old.mkdir(parents=True)
        module.atomic_json(old / 'draft.json', {'candidates': {'prompts': ['old design']}})
        module.atomic_json(old / 'latest-critique.json', {'approved': False, 'issues': ['vary the silhouette']})
        (folder / 'attempt-0034').mkdir()
        self.batch.state['planning']['category-001-00000'] = {'attempts': 34, 'error': 'connection refused'}
        requests = []
        def launch(key, command, log):
            requests.append(module.read_json(Path(command[command.index('--request') + 1])))
            log.write_text('planner failed semantic validation')
            self.batch.live[key] = (Process(1), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch):
            self.batch.plan(one_chunk=True)
        self.assertEqual(requests[0]['retry_index'], 34)
        self.assertEqual(requests[0]['previous_candidates'], {'prompts': ['old design']})
        self.assertEqual(requests[0]['rejection_feedback'], ['vary the silhouette'])

    def test_dry_run_no_process_calls_and_safe_name(self):
        with patch.object(module, 'find_loop') as find, patch.object(module.subprocess, 'Popen') as popen:
            self.assertEqual(module.main([str(self.input), '--output', str(self.root), '--name', 'dry', '--dry-run']), 0)
            find.assert_not_called()
            popen.assert_not_called()
        self.assertEqual(module.read_json(self.root / 'dry/progress.json')['totals']['requested'], 3)
        self.args.name = '../unsafe'
        with self.assertRaises(ValueError):
            module.prepare(self.args)

    def test_exclusive_lock_and_no_overwrite(self):
        self.batch.acquire()
        other = module.Batch(self.batch.path, self.args, Path('/fake'))
        with self.assertRaises(ValueError):
            other.acquire()
        with self.assertRaises(FileExistsError):
            module.prepare(self.args)

    def test_plans_exact_counts_and_resume_skips_cached(self):
        self.plan()
        self.assertEqual(len(self.batch.state['tasks']), 3)
        self.assertEqual(len({t['id'] for t in self.batch.state['tasks']}), 3)
        self.assertTrue(self.batch.state['tasks'][0]['id'].startswith('001-museum-0001-'))
        with patch.object(self.batch, 'launch') as launch:
            self.assertTrue(self.batch.plan())
            launch.assert_not_called()
        # Simulate accepted planner output persisted just before queue checkpoint.
        self.batch.state['tasks'] = []
        with patch.object(self.batch, 'launch') as launch:
            self.assertTrue(self.batch.plan())
            launch.assert_not_called()
        self.assertEqual(len(self.batch.state['tasks']), 3)

    def test_duplicate_prompts_retry_but_never_enqueue(self):
        self.args.max_attempts = 2
        def launch(key, command, log):
            output = Path(command[command.index('--output')+1])
            module.atomic_json(output, {'prompts': [{'name': 'same', 'prompt': 'same', 'diversity': {}}]*2})
            self.batch.live[key] = (Process(), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch), patch.object(self.batch, 'wait_retry'):
            self.assertFalse(self.batch.plan())
        self.assertEqual(self.batch.state['tasks'], [])
        self.assertEqual(self.batch.state['status'], 'needs_attention')

    def make_run(self, task, index=1, accepted=False):
        run = self.batch.path / 'scenes' / f'{task["id"]}-{index:03d}'
        run.mkdir(exist_ok=True)
        module.atomic_json(run / 'run-report.json', {'status': 'accepted' if accepted else 'failed',
                          'active_minutes': 1.2, 'reported_tokens': 30, 'model_attempts': 2, 'models_used': ['test-model']})
        if accepted:
            (run / 'final').mkdir(exist_ok=True)
            for file in ('manifest.json', 'scene.blend', 'scene.glb'):
                (run / 'final' / file).write_text('fixture')
        return run

    def test_cleanup_preserves_exports_and_historical_usage(self):
        self.plan()
        task = self.batch.state['tasks'][0]
        failed = self.make_run(task, 1)
        good = self.make_run(task, 2, accepted=True)
        # Even a newer failed run with export files must not replace an accepted result.
        newer = self.make_run(task, 3, accepted=True)
        report = module.read_json(newer / 'run-report.json')
        report['status'] = 'failed'
        module.atomic_json(newer / 'run-report.json', report)
        module.update_gallery(self.batch.path, [task])
        dataset = module.read_json(self.batch.path / 'dataset.json')['scenes']
        self.assertEqual(len(dataset), 1)
        self.assertIn(good.name, dataset[0]['blend'])
        self.batch.save()
        before = self.batch.state['totals']['reported_tokens']
        self.assertEqual(self.batch.archive_superseded(task), 2)
        self.assertEqual(self.batch.archive_superseded(task), 0)
        self.assertEqual(self.batch.runs(task), [good])
        self.assertTrue((self.batch.path / 'attempt-history' / failed.name).exists())
        self.batch.save()
        self.assertEqual(self.batch.state['totals']['reported_tokens'], before)
        (good / 'final/scene.glb').write_bytes(b'')
        module.update_gallery(self.batch.path, [task])
        self.assertEqual(module.read_json(self.batch.path / 'dataset.json')['scenes'], [])

    def test_cleanup_keeps_resumable_attempt_and_never_moves_live_task(self):
        self.plan()
        task = self.batch.state['tasks'][0]
        old = self.make_run(task, 1)
        latest = self.make_run(task, 2)
        os.utime(old, ns=(1, 1))
        self.batch.live[task['id']] = (Process(), io.StringIO(), 0)
        self.assertEqual(self.batch.archive_superseded(task, fresh=True), 0)
        self.batch.live.clear()
        self.assertEqual(self.batch.archive_superseded(task), 1)
        self.assertEqual(self.batch.runs(task), [latest])
        self.assertEqual(self.batch.archive_superseded(task, fresh=True), 1)
        self.assertEqual(self.batch.runs(task), [])

    def test_accepted_skip_requires_artifacts(self):
        self.plan()
        for task in self.batch.state['tasks']:
            self.make_run(task, accepted=True)
        with patch.object(self.batch, 'launch') as launch:
            self.assertTrue(self.batch.generate())
            launch.assert_not_called()
        totals = module.read_json(self.batch.path / 'progress.json')['totals']
        self.assertEqual(totals['accepted'], 3)
        self.assertEqual(totals['reported_tokens'], 90)
        run = self.batch.runs(self.batch.state['tasks'][0])[0]
        (run / 'final/scene.glb').unlink()
        self.assertFalse(module.accepted(run))

    def test_failed_run_is_resumed_then_redesigned_with_feedback(self):
        self.plan()
        self.batch.state['tasks'] = self.batch.state['tasks'][:1]
        task = self.batch.state['tasks'][0]
        commands = []
        def launch(key, command, log):
            commands.append(command)
            log.write_text('validation failed: furniture overlap')
            if len(commands) in (1, 4):
                self.make_run(task, len(commands), accepted=len(commands) == 4)
            self.batch.live[key] = (Process(0 if len(commands) == 4 else 1), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch), patch.object(self.batch, 'delay', return_value=0), patch.object(module.time, 'sleep'):
            self.assertTrue(self.batch.generate())
        self.assertEqual([cmd[2] for cmd in commands], ['run', 'resume', 'resume', 'run'])
        self.assertEqual(task['redesigns'], 1)
        self.assertIn('furniture overlap', self.batch.scene_task(task)['prompt'])
        self.assertNotIn('furniture overlap', task['prompt'])
        self.assertEqual(len(self.batch.runs(task)), 1)
        self.assertEqual(len(list((self.batch.path / "attempt-history").iterdir())), 1)

    def test_transient_errors_keep_same_run_and_finite_attempt_cap(self):
        self.plan()
        self.batch.state['tasks'] = self.batch.state['tasks'][:1]
        task = self.batch.state['tasks'][0]
        self.make_run(task)
        commands = []
        def launch(key, command, log):
            commands.append(command)
            log.write_text('sse error: Transport error; one or more scenes are incomplete')
            self.batch.live[key] = (Process(1), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch), patch.object(self.batch, 'doctor', return_value=True), patch.object(self.batch, 'delay', return_value=0), patch.object(module.time, 'sleep'):
            self.assertFalse(self.batch.generate())
        self.assertEqual(len(commands), 4)
        self.assertTrue(all(c[2] == 'resume' for c in commands))
        self.assertEqual(task['redesigns'], 0)
        self.assertEqual(task['status'], 'needs_attention')

    def test_keyboard_interrupt_preserves_resume_state(self):
        self.plan()
        self.batch.save()
        with patch.object(module, 'find_loop', return_value=Path('/fake')), patch.object(module.Batch, 'doctor', side_effect=KeyboardInterrupt):
            self.assertEqual(module.main(['--resume', str(self.batch.path)]), 130)
        state = module.read_json(self.batch.path / 'progress.json')
        self.assertEqual(state['status'], 'paused')
        self.assertEqual(len(state['tasks']), 3)

    def test_stop_terminates_process_group(self):
        process = types.SimpleNamespace(pid=555, poll=lambda: None, wait=lambda timeout=None: 0)
        stream = io.StringIO()
        self.batch.live['scene'] = (process, stream, 0)
        with patch.object(module.os, 'killpg') as kill:
            self.batch.stop()
        self.assertEqual(kill.call_args_list[0].args, (555, module.signal.SIGTERM))
        self.assertEqual(kill.call_args_list[-1].args, (555, module.signal.SIGKILL))
        self.assertTrue(stream.closed)
        self.assertEqual(self.batch.live, {})

    def test_seed_stable_per_scene_changes_on_redesign(self):
        self.plan()
        a, b = self.batch.state['tasks'][:2]
        original = self.batch.scene_task(a)['seed']
        self.assertEqual(original, self.batch.scene_task(a)['seed'])
        self.assertNotEqual(original, self.batch.scene_task(b)['seed'])
        a['redesigns'] += 1
        self.assertNotEqual(original, self.batch.scene_task(a)['seed'])

    def test_invalid_cached_plan_is_quarantined_not_hot_looped(self):
        folder = self.batch.path / 'planning/category-001-00000'
        folder.mkdir()
        module.atomic_json(folder / 'accepted.json', {'prompts': []})
        with patch.object(self.batch, 'wait_retry'):
            self.plan()
        self.assertEqual(len(list(folder.glob('invalid-*.json'))), 1)

    def test_orphan_worker_blocks_duplicate_scheduler(self):
        self.batch.state['live_processes'] = [{'key': 'scene', 'pgid': 444}]
        with patch.object(module.os, 'killpg'):
            with self.assertRaisesRegex(ValueError, 'still alive'):
                self.batch.acquire()

    def test_accepted_resume_is_offline(self):
        self.plan()
        for task in self.batch.state['tasks']:
            self.make_run(task, accepted=True)
        self.batch.save()
        with patch.object(module, 'find_loop', return_value=Path('/fake')), patch.object(module.Batch, 'doctor') as doctor:
            self.assertEqual(module.main(['--resume', str(self.batch.path)]), 0)
            doctor.assert_not_called()

    def test_retry_jitter_never_exceeds_cap(self):
        with patch.object(module.random, 'uniform', return_value=1.2):
            self.assertLessEqual(self.batch.delay(5000), self.args.retry_max_seconds)
        for delay in ('nan', 'inf'):
            with patch.object(module, 'find_loop') as find:
                self.assertEqual(module.main(['--resume', str(self.batch.path), '--retry-seconds', delay]), 1)
                find.assert_not_called()

    def test_completed_uncommitted_planner_response_is_recovered(self):
        self.plan()
        tasks = self.batch.state['tasks']
        self.batch.state['tasks'] = []
        for accepted in (self.batch.path / 'planning').glob('*/accepted.json'):
            accepted.unlink()
        # A finite cap must not prevent consuming an already-paid completed response.
        self.args.max_attempts = 1
        with patch.object(self.batch, 'launch') as launch:
            self.assertTrue(self.batch.plan())
            launch.assert_not_called()
        self.assertEqual([t['id'] for t in tasks], [t['id'] for t in self.batch.state['tasks']])

    def test_corrupt_cache_does_not_bypass_attempt_cap(self):
        folder = self.batch.path / 'planning/category-001-00000'
        folder.mkdir()
        (folder / 'accepted.json').write_text('{broken')
        self.batch.state['planning']['category-001-00000'] = {'attempts': 4}
        with patch.object(self.batch, 'launch') as launch:
            self.assertFalse(self.batch.plan())
            launch.assert_not_called()
        self.assertEqual(len(list(folder.glob('invalid-*.json'))), 1)

    def test_malformed_planner_rows_retry_without_crashing(self):
        self.args.max_attempts = 2
        def launch(key, command, log):
            module.atomic_json(Path(command[command.index('--output')+1]), {'prompts': ['invalid', None]})
            self.batch.live[key] = (Process(), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch), patch.object(self.batch, 'wait_retry'):
            self.assertFalse(self.batch.plan())
        self.assertEqual(self.batch.state['planning']['category-001-00000']['attempts'], 2)

    def test_sigterm_handler_is_installed_when_imported_and_restored(self):
        self.plan()
        self.batch.save()
        previous = module.signal.getsignal(module.signal.SIGTERM)
        def interrupt(batch):
            module.signal.getsignal(module.signal.SIGTERM)(module.signal.SIGTERM, None)
        with patch.object(module, 'find_loop', return_value=Path('/fake')), patch.object(module.Batch, 'doctor', new=interrupt):
            self.assertEqual(module.main(['--resume', str(self.batch.path)]), 130)
        self.assertEqual(module.signal.getsignal(module.signal.SIGTERM), previous)
        self.assertEqual(module.read_json(self.batch.path / 'progress.json')['status'], 'paused')

    def test_stop_kills_descendants_even_when_controller_already_exited(self):
        process = types.SimpleNamespace(pid=555, poll=lambda: 0, wait=lambda timeout=None: 0)
        self.batch.live['scene'] = (process, io.StringIO(), 0)
        with patch.object(module.os, 'killpg') as kill:
            self.batch.stop()
        self.assertEqual(kill.call_args_list[-1].args, (555, module.signal.SIGKILL))

    def test_heartbeat_indexes_runs_once_and_exposes_usage_caveat(self):
        self.plan()
        for task in self.batch.state['tasks']:
            self.make_run(task)
        with patch.object(self.batch, 'runs', side_effect=AssertionError('per-task directory scan')):
            self.batch.save()
        self.assertEqual(self.batch.state['totals']['reported_tokens'], 90)
        self.assertIn('unavailable', self.batch.state['totals']['usage_note'])
        self.assertEqual(self.batch.state['totals']['status_counts'], {'queued': 3})

    def test_finite_scene_limit_persists_across_resume(self):
        self.plan()
        self.batch.state['tasks'] = self.batch.state['tasks'][:1]
        self.batch.state['tasks'][0].update(status='interrupted', attempts=self.args.max_attempts)
        self.batch.save()
        resumed = module.Batch(self.batch.path, self.args, Path('/fake'))
        with patch.object(resumed, 'launch') as launch:
            self.assertFalse(resumed.generate())
            launch.assert_not_called()

    def test_model_budget_exhaustion_never_redesigns_or_automatically_retries(self):
        self.plan()
        self.batch.state['tasks'] = self.batch.state['tasks'][:1]
        task = self.batch.state['tasks'][0]
        task['attempts'] = 2  # The third failure would normally trigger a fresh design.
        self.args.max_attempts = None
        def launch(key, command, log):
            log.write_text('model budget exhausted at 21 calls/9999 tokens; one or more scenes are incomplete')
            self.make_run(task)
            self.batch.live[key] = (Process(1), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=launch) as launched, patch.object(module.time, 'sleep'):
            self.assertFalse(self.batch.generate())
            self.assertEqual(launched.call_count, 1)
        self.assertEqual(task['redesigns'], 0)
        self.assertEqual(task['blocked_by'], 'model_budget')
        with patch.object(self.batch, 'launch') as launch:
            self.assertFalse(self.batch.generate())
            launch.assert_not_called()
        self.args.retry_budget_blocked = True
        def accepted_launch(key, command, log):
            self.make_run(task, accepted=True)
            self.batch.live[key] = (Process(0), io.StringIO(), 0)
        with patch.object(self.batch, 'launch', side_effect=accepted_launch), patch.object(module.time, 'sleep'):
            self.assertTrue(self.batch.generate())

    def test_worker_cap_with_running_processes(self):
        self.plan()
        maximum = 0
        def launch(key, command, log):
            nonlocal maximum
            self.batch.live[key] = (Process(None), io.StringIO(), 0)
            maximum = max(maximum, len(self.batch.live))
            self.assertLessEqual(len(self.batch.live), self.args.workers)
        def complete_workers(_):
            for task in self.batch.state['tasks']:
                if task['id'] in self.batch.live:
                    self.make_run(task, accepted=True)
                    self.batch.live[task['id']][0].returncode = 0
        with patch.object(self.batch, 'launch', side_effect=launch), patch.object(module.time, 'sleep', side_effect=complete_workers):
            self.assertTrue(self.batch.generate())
        self.assertEqual(maximum, 2)
        self.assertTrue(all(t['attempts'] == 1 for t in self.batch.state['tasks']))

    def test_cli_subprocess_resume_with_fake_binary_and_completed_skip(self):
        self.plan()
        self.make_run(self.batch.state['tasks'][0], accepted=True)
        self.make_run(self.batch.state['tasks'][1])
        self.batch.save()
        fake = self.root / 'fake-loop'
        calls = self.root / 'calls.txt'
        fake.write_text('#!' + sys.executable + '\n' + textwrap.dedent('''\
            import json, os, sys
            from pathlib import Path
            argv = sys.argv[1:]
            command = argv[1]
            with Path(os.environ['FAKE_CALLS']).open('a') as stream:
                stream.write(command + '\\n')
            if command == 'doctor':
                sys.exit(0)
            if command == 'resume':
                run = Path(argv[argv.index('--run') + 1])
            elif command == 'run':
                task = json.loads(Path(argv[argv.index('--tasks') + 1]).read_text())['tasks'][0]
                run = Path(task['overrides']['storage']['output_root']) / (task['id'] + '-fake')
            else:
                raise RuntimeError('Unexpected invocation: ' + command)
            final = run / 'final'
            final.mkdir(parents=True, exist_ok=True)
            for filename in ['manifest.json', 'scene.blend', 'scene.glb']:
                (final / filename).write_text('test fixture only')
            (run / 'run-report.json').write_text(json.dumps({'status': 'accepted'}))
            '''))
        fake.chmod(0o755)
        command = [sys.executable, str(module.ROOT / 'generate_scenes.py'), '--resume', str(self.batch.path), '--workers', '2']
        env = {**os.environ, 'LOOP_BIN': str(fake), 'FAKE_CALLS': str(calls)}
        result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertCountEqual(calls.read_text().splitlines(), ['doctor', 'resume', 'run'])
        self.assertEqual(module.read_json(self.batch.path / 'progress.json')['totals']['accepted'], 3)
        previous_calls = calls.read_text()
        result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(calls.read_text(), previous_calls)


if __name__ == '__main__':
    unittest.main()
