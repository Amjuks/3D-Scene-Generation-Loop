#!/usr/bin/env python3
"""Durable category-to-scene batches. Resume with --resume PATH; Ctrl-C preserves work."""
import argparse
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import random
import re
import signal
import subprocess
import sys
import time
import uuid

from generate_scenes import ROOT, find_loop, update_gallery, accepted_run


def atomic_json(path, value):
    temporary = path.with_name(path.name + '.tmp')
    with temporary.open('w', encoding='utf-8') as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def read_json(path, default=None):
    try:
        return json.loads(path.read_text(encoding='utf-8'))
    except (FileNotFoundError, json.JSONDecodeError):
        return default


def categories_file(path):
    """Deliberately restricted YAML: flat names and positive decimal counts only."""
    result = {}
    for number, raw in enumerate(path.read_text(encoding='utf-8-sig').splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith('#'):
            continue
        match = re.fullmatch(r'([^\r\n]+?):\s*([0-9]+)\s*(?:#.*)?', raw)
        if not match or raw[:1].isspace():
            raise ValueError(f'Line {number}: expected category_name: positive_integer (flat YAML only)')
        name, count = match[1].strip(), int(match[2])
        if name.startswith('"') and name.endswith('"'):
            name = json.loads(name)
        elif name.startswith("'") and name.endswith("'"):
            name = name[1:-1].replace("''", "'")
        elif any(char in name for char in ':{}[]&*!#'):
            raise ValueError(f'Line {number}: quote this category name')
        if not name.strip() or count < 1 or name in result:
            raise ValueError(f'Line {number}: categories must be unique, nonempty, with positive counts')
        result[name] = count
    if not result:
        raise ValueError('Category file is empty')
    return result


def slug(value):
    return re.sub(r'[^a-z0-9]+', '-', value.lower()).strip('-')[:60] or 'scene'


def fingerprint(prompt):
    return hashlib.sha256(' '.join(prompt.lower().split()).encode()).hexdigest()


accepted = accepted_run


def failure_kind(error):
    """Classify the actual failure, never the CLI's generic incomplete summary."""
    error = error.lower()
    if 'model budget exhausted' in error:
        return 'model_budget'
    if any(x in error for x in ('configured soket model not found', 'requires image input', 'http 400:', 'http 404:', 'http 422:')):
        return 'configuration'
    if any(x in error for x in ('transport', 'connection refused', 'connection reset', 'error sending',
                                'timeout', 'timed out', 'rate limit', '429', '503', 'http 408:', 'http 500:', 'http 502:', 'http 504:', 'sse error')):
        return 'transient'
    if any(x in error for x in ('validation:', 'validation failed', 'cannot fit', 'outside room', 'overlap', 'no legal', 'node budget')):
        return 'structural'
    return 'unknown'


class Batch:
    def __init__(self, path, args, binary):
        self.path, self.args, self.binary = path, args, binary
        self.state = read_json(path / 'progress.json')
        self.live = {}
        self.lock = None

    def acquire(self):
        self.lock = (self.path / '.batch.lock').open('a+')
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            self.lock.close()
            raise ValueError('Another scheduler owns this batch; stop it before resuming')
        for worker in self.state.get('live_processes', []):
            try:
                os.killpg(worker['pgid'], 0)
            except ProcessLookupError:
                continue
            except PermissionError:
                pass
            self.lock.close()
            raise ValueError(f"A previous worker process group {worker['pgid']} is still alive. Let it finish or stop that group before resuming; no duplicate workers launched.")
        self.lock.seek(0)
        self.lock.truncate()
        self.lock.write(str(os.getpid()))
        self.lock.flush()

    def runs(self, task):
        return sorted((self.path / 'scenes').glob(task['id'] + '-*'), key=lambda p: p.stat().st_mtime_ns)

    def archive_superseded(self, task, fresh=False):
        """Keep one accepted result or the latest resumable attempt, never a live run."""
        if task['id'] in self.live:
            return 0
        runs = self.runs(task)
        successes = sorted(run for run in runs if accepted(run))
        keep = successes[-1] if successes else (runs[-1] if runs and not fresh else None)
        obsolete = [run for run in runs if run != keep]
        archive = self.path / 'attempt-history'
        for run in obsolete:
            destination = archive / run.name
            if destination.exists():
                raise ValueError(f'Archive destination already exists: {destination}')
        if obsolete:
            archive.mkdir(exist_ok=True)
        for run in obsolete:
            run.rename(archive / run.name)
        return len(obsolete)

    def run_index(self):
        """Scan scenes and archived attempts once per heartbeat for historical metrics."""
        index = {task['id']: [] for task in self.state['tasks']}
        for run in [*(self.path / 'scenes').iterdir(), *(self.path / 'attempt-history').glob('*')]:
            if not run.is_dir():
                continue
            prefix = run.name
            while '-' in prefix:
                prefix = prefix.rsplit('-', 1)[0]
                if prefix in index:
                    index[prefix].append(run)
                    break
        return index

    def save(self):
        state = self.state
        state['live_processes'] = [{'key': key, 'pgid': process.pid} for key, (process, _, _) in self.live.items()]
        state['updated_at'] = datetime.now(timezone.utc).isoformat()
        state['wall_minutes'] = (time.time() - state['created_epoch']) / 60
        totals = {'scene_active_minutes': 0, 'reported_tokens': 0, 'model_attempts': 0,
                  'accepted': 0, 'requested': sum(state['categories'].values())}
        models = set()
        runs = self.run_index()
        for task in state['tasks']:
            metrics = [read_json(run / 'run-report.json', {}) for run in runs[task['id']]]
            task['metrics'] = {
                'active_minutes': sum(m.get('active_minutes', 0) for m in metrics),
                'reported_tokens': sum(m.get('reported_tokens', 0) or 0 for m in metrics),
                'model_attempts': sum(m.get('model_attempts', 0) for m in metrics),
                'models_used': sorted({v for m in metrics for v in m.get('models_used', [])}),
            }
            totals['scene_active_minutes'] += task['metrics']['active_minutes']
            totals['reported_tokens'] += task['metrics']['reported_tokens']
            totals['model_attempts'] += task['metrics']['model_attempts']
            totals['accepted'] += task['status'] == 'accepted'
            models.update(task['metrics']['models_used'])
        # Planner attempts keep separate telemetry directories, including failed calls.
        planner_reports = [read_json(p, {}) or {} for p in (self.path / 'planning').glob('**/run-report.json')]
        totals['planner_reported_tokens'] = sum(p.get('reported_tokens', 0) or 0 for p in planner_reports)
        totals['planner_model_attempts'] = sum(p.get('model_attempts', 0) for p in planner_reports)
        totals['planner_active_minutes'] = sum(p.get('active_minutes', 0) for p in planner_reports)
        for report in planner_reports:
            models.update(report.get('models_used', []))
        totals['reported_tokens'] += totals['planner_reported_tokens']
        totals['model_attempts'] += totals['planner_model_attempts']
        totals['models_used'] = sorted(models)
        totals['cost'] = None
        totals['usage_note'] = 'Provider-reported tokens only; usage for failed calls may be unavailable. Cost is unknown.'
        totals['status_counts'] = {}
        for task in state['tasks']:
            totals['status_counts'][task['status']] = totals['status_counts'].get(task['status'], 0) + 1
        state['totals'] = totals
        state['category_progress'] = {category: {'requested': count,
            'planned': sum(t['category'] == category for t in state['tasks']),
            'accepted': sum(t['category'] == category and t['status'] == 'accepted' for t in state['tasks']),
            'running': sum(t['category'] == category and t['status'] == 'running' for t in state['tasks'])}
            for category, count in state['categories'].items()}
        atomic_json(self.path / 'progress.json', state)
        lines = [f"# {state['name']}", '', f"Status: {state['status']} | Accepted: {totals['accepted']}/{totals['requested']}",
                 f"Wall minutes: {state['wall_minutes']:.2f} | Scene active minutes: {totals['scene_active_minutes']:.2f}",
                 f"Reported tokens: {totals['reported_tokens']} | Model attempts: {totals['model_attempts']}",
                 f"Models: {', '.join(totals['models_used']) or 'none yet'}", '',
                 'Tokens include available provider usage only; failed-call usage and cost may be unknown.',
                 'Scene active minutes sum workers and can exceed wall minutes. Wall time includes pauses.', '',
                 '| Scene | Status | Attempts | Active minutes | Tokens |', '|---|---|---:|---:|---:|']
        if state.get('retry_limits'):
            limits = state['retry_limits']
            lines.insert(4, f"Retry limits: {limits['max_attempts'] or 'unlimited'} attempts per scene/chunk; {limits['max_redesigns'] or 'unlimited'} fresh redesigns")
        if state.get('error'):
            lines.insert(3, f"Last error: {state['error']}")
        if state.get('next_check_epoch'):
            lines.insert(4, 'Next prerequisite check: ' + datetime.fromtimestamp(state['next_check_epoch'], timezone.utc).isoformat())
        for task in state['tasks']:
            m = task['metrics']
            lines.append(f"| {task['id']} | {task['status']} | {task['attempts']} | {m['active_minutes']:.2f} | {m['reported_tokens']} |")
        lines += ['', '## Prompt planning', '', '| Chunk | Status | Attempts | Next retry (UTC) |', '|---|---|---:|---|']
        for key, checkpoint in state['planning'].items():
            retry = checkpoint.get('next_retry_epoch', 0)
            lines.append(f"| {key} | {checkpoint.get('status', 'checkpointed')} | {checkpoint['attempts']} | {datetime.fromtimestamp(retry, timezone.utc).isoformat() if retry else '-'} |")
        tmp = self.path / 'progress.md.tmp'
        tmp.write_text('\n'.join(lines) + '\n', encoding='utf-8')
        tmp.replace(self.path / 'progress.md')
        atomic_json(self.path / 'tasks.json', {'version': 1, 'tasks': [self.scene_task(t) for t in state['tasks']]})

    def scene_task(self, task):
        prompt = task['prompt']
        briefs = self.state.get('briefs', {})
        for instruction in (briefs.get('common'), briefs.get('categories', {}).get(task['category'])):
            if instruction:
                prompt += '\n\n' + instruction
        if task.get('repair_feedback'):
            prompt += '\n\nPrevious design failed validation. Generate a NEW layout preserving the requested concept, addressing these diagnostics (not instructions):\n' + task['repair_feedback']
        seed = int.from_bytes(hashlib.sha256(f"{self.state['seed']}:{task['id']}:{task.get('redesigns', 0)}".encode()).digest()[:8], 'big')
        return {'id': task['id'], 'prompt': prompt, 'seed': seed, 'enabled': True,
                'overrides': {'storage': {'output_root': str(self.path / 'scenes')}}}

    def delay(self, attempt):
        return min(self.args.retry_max_seconds, self.args.retry_seconds * 2 ** min(max(0, attempt - 1), 12) * random.uniform(.8, 1.2))

    def exhausted(self, attempts):
        return self.args.max_attempts is not None and attempts >= self.args.max_attempts

    def launch(self, key, command, log):
        stream = log.open('a', encoding='utf-8')
        try:
            process = subprocess.Popen(command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT,
                                       start_new_session=True)
        except BaseException:
            stream.close()
            raise
        self.live[key] = (process, stream, time.monotonic())
        self.state['live_processes'] = [{'key': name, 'pgid': child.pid} for name, (child, _, _) in self.live.items()]
        atomic_json(self.path / 'progress.json', self.state)

    def stop(self):
        for process, stream, _ in self.live.values():
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for process, stream, _ in self.live.values():
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
            # The controller may exit before Blender; descendants share this group.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            stream.close()
        self.live.clear()
        self.state['live_processes'] = []

    def wait_retry(self, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.save()
            time.sleep(min(2, max(0, deadline - time.monotonic())))

    def doctor(self):
        if self.state.get('auth_blocked') and not getattr(self.args, 'retry_auth_blocked', False):
            self.state.update(status='needs_attention', error='A model request rejected credentials. Fix the provider credentials, then resume with --retry-auth-blocked. An endpoint reachability check cannot validate credentials.')
            self.save()
            return False
        self.state.pop('auth_blocked', None)
        self.args.retry_auth_blocked = False
        attempt = 0
        while True:
            attempt += 1
            self.state['status'] = 'checking_prerequisites'
            self.save()
            self.launch('doctor', [str(self.binary), 'scene', 'doctor', '--config', self.state['config']], self.path / 'preflight.log')
            process, stream, started = self.live['doctor']
            while process.poll() is None and time.monotonic() - started < 90:
                self.save()
                time.sleep(1)
            if process.poll() is None:
                self.stop()
                code = 124
            else:
                code = process.returncode
                stream.close()
                del self.live['doctor']
            if code == 0:
                self.state.pop('error', None)
                self.state.pop('next_check_epoch', None)
                self.state['prerequisite_failures'] = 0
                return True
            detail = (self.path / 'preflight.log').read_text(errors='replace')[-2500:]
            if failure_kind(detail) == 'configuration':
                self.state.update(status='needs_attention', error=detail)
                self.save()
                return False
            endpoint_down = any(s in detail.lower() for s in ('endpoint', 'connection refused', 'connection reset', 'unreachable', 'timed out'))
            delay = max(30, self.delay(attempt))
            self.state.update(status='waiting_endpoint' if endpoint_down else 'waiting_prerequisites',
                              prerequisite_failures=attempt, next_check_epoch=time.time() + delay,
                              error=f'Preflight failed; no model generation will be attempted until checks recover. {detail}')
            self.save()
            if self.exhausted(attempt):
                return False
            self.wait_retry(delay)

    def plan(self, one_chunk=False):
        self.state['status'] = 'planning'
        categories = list(enumerate(self.state['categories'].items(), 1))
        if one_chunk:
            cursor = self.state.get('planning_cursor', 0) % len(categories)
            categories = categories[cursor:] + categories[:cursor]
        for category_index, (category, count) in categories:
            while len([t for t in self.state['tasks'] if t['category'] == category]) < count:
                if self.state.get('recheck_prerequisites'):
                    if one_chunk:
                        return None
                    if not self.doctor():
                        return False
                    self.state.pop('recheck_prerequisites', None)
                existing = [t for t in self.state['tasks'] if t['category'] == category]
                offset = len(existing)
                key = f'category-{category_index:03d}-{offset:05d}'
                checkpoint = self.state['planning'].setdefault(key, {'attempts': 0})
                if one_chunk:
                    self.state['planning_cursor'] = category_index % len(categories)
                folder = self.path / 'planning' / key
                folder.mkdir(parents=True, exist_ok=True)
                recoverable = (folder / 'accepted.json').is_file() or (folder / f"attempt-{checkpoint['attempts']:04d}" / 'response.json').is_file()
                if checkpoint.get('blocked_by') == 'configuration' and not recoverable:
                    if one_chunk:
                        break
                    return False
                if one_chunk and not recoverable and (self.exhausted(checkpoint['attempts']) or time.time() < checkpoint.get('next_retry_epoch', 0)):
                    break
                # A successful response is committed before scheduling. A crash in this gap reuses it.
                response_path = folder / 'accepted.json'
                response = read_json(response_path)
                if response is None and response_path.exists():
                    response_path.replace(folder / f'invalid-{uuid.uuid4().hex}.json')
                if response is None and checkpoint['attempts']:
                    # A killed scheduler may not have committed the completed planner output yet.
                    response_path = folder / f"attempt-{checkpoint['attempts']:04d}" / 'response.json'
                    response = read_json(response_path)
                if response is None:
                    if self.exhausted(checkpoint['attempts']):
                        self.state.update(status='needs_attention', error=f'Planner attempt limit reached for {category}; resume with larger --max-attempts or explicitly use 0 for unlimited retries')
                        self.save()
                        if one_chunk:
                            break
                        return False
                    if checkpoint['attempts']:
                        # Transport-only attempts often have no draft. Recover the
                        # newest actual candidates, not just the latest attempt folder.
                        prior = sorted(folder.glob('attempt-*/response-planning'), key=lambda p: p.parent.name, reverse=True)
                        for old in prior:
                            draft = read_json(old / 'draft.json', {}) or {}
                            if not isinstance(draft, dict) or draft.get('candidates') is None:
                                continue
                            critique = read_json(old / 'latest-critique.json', {}) or {}
                            checkpoint['previous_candidates'] = draft['candidates']
                            if draft.get('rejection_feedback') is not None:
                                checkpoint['rejection_feedback'] = draft['rejection_feedback']
                            elif isinstance(critique, dict) and critique.get('approved') is False:
                                checkpoint['rejection_feedback'] = critique.get('issues', critique)
                            break
                    checkpoint['attempts'] += 1
                    checkpoint['status'] = 'running'
                    attempt = folder / f"attempt-{checkpoint['attempts']:04d}"
                    attempt.mkdir(exist_ok=True)
                    request = {'category': category, 'count': min(5, count-offset),
                               'offset': offset, 'seed': int.from_bytes(hashlib.sha256(f"{self.state['seed']}:{category}".encode()).digest()[:8], 'big'),
                               'retry_index': checkpoint['attempts'] - 1,
                               'briefs': {'common': self.state.get('briefs', {}).get('common', ''),
                                          'category': self.state.get('briefs', {}).get('categories', {}).get(category, '')},
                               'previous_candidates': checkpoint.get('previous_candidates'),
                               'rejection_feedback': checkpoint.get('rejection_feedback') or (checkpoint.get('error') if checkpoint.get('previous_candidates') is None else None),
                               'existing': [{k: t[k] for k in ('name', 'prompt', 'diversity')} for t in self.state['tasks']]}
                    atomic_json(attempt / 'request.json', request)
                    self.save()
                    self.launch('planner', [str(self.binary), 'scene', 'plan-prompts', '--config', self.state['config'],
                                           '--request', str(attempt / 'request.json'), '--output', str(attempt / 'response.json')], attempt / 'planner.log')
                    process, stream, _ = self.live['planner']
                    while process.poll() is None:
                        self.save()
                        time.sleep(2)
                    stream.close()
                    del self.live['planner']
                    response_path = attempt / 'response.json'
                    response = read_json(response_path) if process.returncode == 0 else None
                    draft = read_json(attempt / 'response-planning' / 'draft.json', {}) or {}
                    if draft.get('candidates') is not None:
                        checkpoint['previous_candidates'] = draft['candidates']
                    if draft.get('rejection_feedback') is not None:
                        checkpoint['rejection_feedback'] = draft['rejection_feedback']
                    if process.returncode:
                        detail = (attempt / 'planner.log').read_text(errors='replace')[-6000:] if (attempt / 'planner.log').exists() else ''
                        checkpoint['error'] = detail.strip() or f'Planner exited with code {process.returncode}; see {attempt / "planner.log"}'
                        if any(s in checkpoint['error'].lower() for s in ('unauthorized', 'invalid api key', 'invalid_api_key', 'authentication failed', 'status: 401', 'status: 403')):
                            self.state['auth_blocked'] = True
                            self.state['recheck_prerequisites'] = True
                        if any(s in checkpoint['error'].lower() for s in ('transport', 'connection refused', 'connection reset', 'error sending', 'timeout', 'timed out', 'credential', 'unauthorized', 'api key', '429', '503')):
                            self.state['recheck_prerequisites'] = True
                try:
                    if not isinstance(response, dict) or not isinstance(response.get('prompts'), list):
                        raise ValueError(checkpoint.get('error') or 'Planner returned no valid prompts object; see planner.log')
                    rows = response['prompts']
                    seen = {fingerprint(t['prompt']) for t in self.state['tasks']}
                    if len(rows) != min(5, count-offset):
                        raise ValueError('Planner returned wrong prompt count')
                    for row in rows:
                        if not isinstance(row, dict) or not isinstance(row.get('name'), str) or not row['name'].strip() or not isinstance(row.get('prompt'), str) or not row['prompt'].strip() or not isinstance(row.get('diversity'), dict):
                            raise ValueError('Planner response is missing name, prompt, or diversity object')
                        digest = fingerprint(row['prompt'])
                        if digest in seen:
                            raise ValueError('Duplicate prompt; requesting a new planning attempt')
                        seen.add(digest)
                except (ValueError, TypeError, KeyError) as error:
                    if response_path.exists():
                        response_path.replace(response_path.with_name(f'invalid-{uuid.uuid4().hex}.json'))
                    checkpoint['error'] = str(error)
                    configuration_error = failure_kind(str(error)) == 'configuration'
                    if configuration_error:
                        checkpoint['blocked_by'] = 'configuration'
                    checkpoint['status'] = 'needs_attention' if configuration_error or self.exhausted(checkpoint['attempts']) else 'retry_wait'
                    checkpoint['next_retry_epoch'] = time.time() + self.delay(checkpoint['attempts'])
                    self.state['error'] = f'{category}: {error}; see planning logs'
                    self.save()
                    if one_chunk:
                        return None
                    if configuration_error or self.exhausted(checkpoint['attempts']):
                        self.state['status'] = 'needs_attention'
                        self.save()
                        return False
                    self.wait_retry(self.delay(checkpoint['attempts']))
                    continue
                atomic_json(folder / 'accepted.json', response)
                checkpoint.update(status='accepted', next_retry_epoch=0)
                checkpoint.pop('error', None)
                for index, row in enumerate(rows, offset+1):
                    self.state['tasks'].append({**row, 'id': f'{category_index:03d}-{slug(category)}-{index:04d}-{slug(row["name"])}',
                                                'category': category, 'status': 'queued', 'attempts': 0,
                                                'redesigns': 0, 'next_retry_epoch': 0})
                self.state.pop('error', None)
                self.save()
                if one_chunk:
                    return True
        if one_chunk:
            return None
        return True

    def generate(self, incremental=False):
        self.state['status'] = 'generating'
        for task in self.state['tasks']:
            if any(accepted(run) for run in self.runs(task)):
                task['status'] = 'accepted'
                self.archive_superseded(task)
            elif task.get('blocked_by') == 'model_budget' and not getattr(self.args, 'retry_budget_blocked', False):
                task['status'] = 'needs_attention'
            elif task.get('blocked_by') == 'configuration' and not getattr(self.args, 'retry_config_blocked', False):
                task['status'] = 'needs_attention'
            elif task.get('blocked_by') == 'redesign_limit' and self.args.max_redesigns and task.get('redesigns', 0) >= self.args.max_redesigns:
                task['status'] = 'needs_attention'
            elif task['status'] in ('running', 'accepted', 'interrupted', 'needs_attention'):
                task['status'] = 'queued'
                task.pop('blocked_by', None)
        while True:
            if self.state.get('recheck_prerequisites') and not self.live:
                if not self.doctor():
                    return False
                self.state.pop('recheck_prerequisites', None)
                self.state['status'] = 'generating'
            for task in self.state['tasks']:
                key = task['id']
                if key in self.live:
                    process, stream, _ = self.live[key]
                    if process.poll() is None:
                        continue
                    stream.close()
                    del self.live[key]
                    runs = self.runs(task)
                    if any(accepted(run) for run in runs):
                        task['status'] = 'accepted'
                        task.pop('error', None)
                        self.archive_superseded(task)
                    else:
                        task['status'] = 'retry_wait'
                        log = self.path / 'logs' / f'{key}-{task["attempts"]:04d}.log'
                        error = log.read_text(errors='replace')[-12000:]
                        task['error'] = error[-1500:]
                        kind = failure_kind(error)
                        budget_blocked = kind == 'model_budget'
                        if kind == 'configuration':
                            task.update(status='needs_attention', blocked_by='configuration')
                        if any(s in error.lower() for s in ('unauthorized', 'invalid api key', 'invalid_api_key', 'authentication failed', 'status: 401', 'status: 403')):
                            self.state['auth_blocked'] = True
                            self.state['recheck_prerequisites'] = True
                        if budget_blocked:
                            task['status'] = 'needs_attention'
                            task['blocked_by'] = 'model_budget'
                            task['error'] += '\nModel spending limit reached. Review the saved run budget before explicitly resuming with --retry-budget-blocked; automatic retries/redesigns are disabled for this task.'
                        if kind == 'transient' or any(s in error.lower() for s in ('credential', 'unauthorized', 'api key', 'disk space', 'no space left', 'connection refused', 'connection reset', 'error sending', 'transport', 'sse error', 'timeout', 'timed out', '429', '503')):
                            self.state['recheck_prerequisites'] = True
                        # Geometry/planning dead ends need a fresh design, not the same cached failure.
                        if kind == 'structural':
                            task['structural_failures'] = task.get('structural_failures', 0) + 1
                        else:
                            task['structural_failures'] = 0
                        if kind == 'structural' and task['structural_failures'] >= 3:
                            task['structural_failures'] = 0
                            if self.args.max_redesigns and task['redesigns'] >= self.args.max_redesigns:
                                task.update(status='needs_attention', blocked_by='redesign_limit')
                            else:
                                task['fresh_next'] = True
                                task['redesigns'] += 1
                                task['repair_feedback'] = error[-2500:]
                        task['next_retry_epoch'] = time.time() + self.delay(task['attempts'])
                    self.save()
                    update_gallery(self.path, self.state['tasks'])
                if task['status'] in ('accepted', 'needs_attention', 'running'):
                    continue
                if self.exhausted(task['attempts']):
                    task['status'] = 'needs_attention'
                    continue
                if len(self.live) >= self.args.workers or time.time() < task.get('next_retry_epoch', 0):
                    continue
                if self.state.get('recheck_prerequisites'):
                    continue
                runs = self.runs(task)
                fresh = task.pop('fresh_next', False)
                if runs and not fresh:
                    command = [str(self.binary), 'scene', 'resume', '--run', str(runs[-1])]
                else:
                    if fresh:
                        self.archive_superseded(task, fresh=True)
                    taskfile = self.path / 'inputs' / (key + '.json')
                    atomic_json(taskfile, {'version': 1, 'tasks': [self.scene_task(task)]})
                    command = [str(self.binary), 'scene', 'run', '--config', self.state['config'], '--tasks', str(taskfile)]
                task['attempts'] += 1
                task['status'] = 'running'
                self.state['status'] = 'generating'
                self.save()
                self.launch(key, command, self.path / 'logs' / f'{key}-{task["attempts"]:04d}.log')
            self.save()
            missing = incremental and len(self.state['tasks']) < sum(self.state['categories'].values())
            if missing and not self.live and not self.state.get('recheck_prerequisites'):
                # Launch ready scene work first. Only spend a planning chunk when no
                # scene worker is active; never gate a ready scene on the full catalog.
                self.plan(one_chunk=True)
                self.save()
                if self.state.get('recheck_prerequisites'):
                    continue
                pending = []
                for index, (category, count) in enumerate(self.state['categories'].items(), 1):
                    offset = sum(t['category'] == category for t in self.state['tasks'])
                    if offset < count:
                        pending.append(self.state['planning'].get(f'category-{index:03d}-{offset:05d}', {'attempts': 0}))
                missing = bool(pending)
                if pending and all(p.get('blocked_by') == 'configuration' or self.exhausted(p['attempts']) for p in pending) and all(t['status'] in ('accepted', 'needs_attention') for t in self.state['tasks']):
                    self.state['status'] = 'needs_attention'
                    self.state['error'] = 'Remaining prompt chunks reached their attempt limit; inspect planning errors, then resume with an explicit larger --max-attempts after reviewing the failures.'
                    self.save()
                    return False
                if missing:
                    self.state['status'] = 'waiting_planning_retry' if pending and all(p.get('next_retry_epoch', 0) > time.time() or self.exhausted(p['attempts']) for p in pending) else 'generating'
            if not missing and not self.live and all(t['status'] in ('accepted', 'needs_attention') for t in self.state['tasks']):
                done = all(t['status'] == 'accepted' for t in self.state['tasks'])
                self.state['status'] = 'accepted' if done else 'needs_attention'
                self.save()
                update_gallery(self.path, self.state['tasks'])
                return done
            time.sleep(2)


def prepare(args):
    if args.resume:
        path = args.resume.expanduser().resolve()
        if not (path / 'progress.json').is_file():
            raise ValueError('Resume path must contain progress.json')
        if args.categories or args.name or args.config or getattr(args, 'briefs', None):
            raise ValueError('--resume uses saved categories, name, config, and briefs; do not supply those again')
        return path
    if args.categories is None:
        raise ValueError('Provide categories.yaml or --resume BATCH')
    categories = categories_file(args.categories)
    briefs = read_json(args.briefs.expanduser().resolve()) if getattr(args, 'briefs', None) else {}
    if not isinstance(briefs, dict) or set(briefs) - {'common', 'categories'} or not isinstance(briefs.get('common', ''), str) or not isinstance(briefs.get('categories', {}), dict):
        raise ValueError('Briefs must be a JSON object with common text and an optional categories mapping')
    if any(k not in categories or not isinstance(v, str) for k, v in briefs.get('categories', {}).items()):
        raise ValueError('Category briefs must contain text keyed by an input category name')
    name = args.name or datetime.now(timezone.utc).strftime('batch-%Y%m%d-%H%M%S') + '-' + uuid.uuid4().hex[:6]
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,100}', name) or name in ('.', '..'):
        raise ValueError('--name must be a safe folder name (letters, numbers, dots, underscores, hyphens)')
    config = read_json(args.config.expanduser().resolve()) if args.config else {
        'version': 1, 'models': {'provider': 'soket', 'default_model': 'gemma-4-31b',
                               'fallback_models': ['gpt', 'qwen3-8-27b'], 'compact_planning': True},
        'backend': {'generator': 'blender', 'variation': 'procedural_pbr_v1', 'preview_renderer': 'cycles', 'exports': ['blend', 'glb']},
        'quality': {'preview_resolution': [1600, 1200], 'preview_samples': 64, 'require_visual_review': False}}
    if not isinstance(config, dict):
        raise ValueError('Config must be an existing JSON object')
    path = args.output.expanduser().resolve() / name
    path.mkdir(parents=True, exist_ok=False)
    for folder in ('scenes', 'planning', 'logs', 'inputs'):
        (path / folder).mkdir()
    config.setdefault('storage', {}).update(output_root=str(path / 'scenes'), asset_library=str(path / 'library'))
    config['storage'].setdefault('min_free_bytes', 2000000000)
    atomic_json(path / 'config.json', config)
    atomic_json(path / 'progress.json', {'version': 1, 'name': name, 'categories': categories,
                                       'seed': args.seed, 'config': str(path / 'config.json'), 'briefs': briefs,
                                       'created_epoch': time.time(), 'status': 'prepared', 'tasks': [], 'planning': {}})
    return path


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('categories', nargs='?', type=Path)
    parser.add_argument('--name')
    parser.add_argument('--output', type=Path, default=ROOT / 'scene-runs')
    parser.add_argument('--resume', type=Path)
    parser.add_argument('--config', type=Path)
    parser.add_argument('--briefs', type=Path, help='JSON with common and category-specific scene constraints; saved for resume')
    parser.add_argument('--workers', type=int, default=1)
    parser.add_argument('--seed', type=int, default=0)
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--plan-only', action='store_true')
    parser.add_argument('--cleanup', action='store_true', help='Archive superseded runs and rebuild accepted-only dataset/gallery; no generation')
    parser.add_argument('--max-attempts', type=int, help='Total attempts per scene/chunk across resumes (default 8; 0 explicitly unlimited)')
    parser.add_argument('--retry-budget-blocked', action='store_true', help='Explicitly retry spending-limit-blocked tasks after reviewing/updating their saved run budgets')
    parser.add_argument('--retry-config-blocked', action='store_true', help='Retry after correcting blocked model configuration, including saved scene snapshots')
    parser.add_argument('--retry-auth-blocked', action='store_true', help='Retry after fixing credentials rejected by a previous model request')
    parser.add_argument('--max-redesigns', type=int, help='Fresh-design recovery limit (default 2; 0 explicitly unlimited)')
    parser.add_argument('--retry-seconds', '--retry-base', type=float, default=10)
    parser.add_argument('--retry-max-seconds', '--retry-cap', type=float, default=300)
    args = parser.parse_args(argv)
    batch = None
    previous_sigterm = signal.getsignal(signal.SIGTERM)
    def interrupt_handler(*_):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupt_handler)
    try:
        if not 0 <= args.seed <= 2**64-1 or args.workers < 1 or args.workers > 16 or (args.max_attempts is not None and args.max_attempts < 0) or (args.max_redesigns is not None and args.max_redesigns < 0) or not all(math.isfinite(v) for v in (args.retry_seconds, args.retry_max_seconds)) or args.retry_seconds <= 0 or args.retry_max_seconds < args.retry_seconds:
            raise ValueError('Require workers 1..16, positive attempts/delays, retry max >= base, nonnegative redesigns')
        if args.cleanup and (not args.resume or args.dry_run or args.plan_only):
            raise ValueError('--cleanup requires --resume and cannot combine with --dry-run or --plan-only')
        binary = None if args.dry_run or args.cleanup else find_loop()
        path = prepare(args)
        batch = Batch(path, args, binary)
        batch.acquire()
        if args.cleanup:
            moved = sum(batch.archive_superseded(task) for task in batch.state['tasks'])
            batch.save()
            update_gallery(batch.path, batch.state['tasks'])
            print(f'Archived {moved} superseded runs to {batch.path / "attempt-history"}')
            return 0
        if args.retry_config_blocked:
            for item in [*batch.state['tasks'], *batch.state['planning'].values()]:
                if item.get('blocked_by') == 'configuration':
                    item.pop('blocked_by')
            args.retry_config_blocked = False
        limits = batch.state.setdefault('retry_limits', {'max_attempts': 8, 'max_redesigns': 2})
        for key in ('max_attempts', 'max_redesigns'):
            if getattr(args, key) is not None:
                limits[key] = getattr(args, key)
            setattr(args, key, limits[key])
        if args.max_attempts == 0:
            args.max_attempts = None
        print(f'Batch: {path}\nProgress: {path / "progress.md"}', flush=True)
        batch.save()
        if args.dry_run:
            print('Prepared category counts; no LLM calls or scenes generated. Resume without --dry-run to start.')
            return 0
        if len(batch.state['tasks']) == sum(batch.state['categories'].values()) and all(
                any(accepted(run) for run in batch.runs(task)) for task in batch.state['tasks']):
            return 0 if batch.generate() else 1
        if not batch.doctor():
            return 1
        if args.plan_only:
            if not batch.plan():
                return 1
            batch.state['status'] = 'planned'
            batch.save()
            return 0
        return 0 if batch.generate(incremental=True) else 1
    except KeyboardInterrupt:
        if batch:
            batch.stop()
            batch.state['status'] = 'paused'
            for task in batch.state['tasks']:
                if task['status'] == 'running':
                    task['status'] = 'interrupted'
            batch.save()
            print(f'Paused safely. Resume: python3 scene_batch.py --resume {batch.path}', file=sys.stderr)
        return 130
    except (OSError, ValueError) as error:
        if batch and batch.lock and not batch.lock.closed:
            batch.stop()
            batch.state.update(status='needs_attention', error=str(error))
            try:
                batch.save()
            except OSError:
                pass
        print(f'Error: {error}', file=sys.stderr)
        return 1
    finally:
        signal.signal(signal.SIGTERM, previous_sigterm)
        if batch:
            batch.stop()
            if batch.lock and not batch.lock.closed:
                batch.lock.close()


if __name__ == '__main__':
    sys.exit(main())
