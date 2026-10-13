#!/usr/bin/env python3
"""Exercise an installed, running FS + Guard + Scope + Pivot stack as its service user.

Requires root for the temporary Guard policy. Restores the installed policy on exit.
Usage: python3 scripts/test-pivot-runtime.py [ARTIFACT_DIR]
"""
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid

root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(tempfile.mkdtemp(prefix="pivot-native-"))
root.mkdir(parents=True, exist_ok=True)
root.chmod(0o755)
daemon = json.loads(Path('/etc/agentcensor/censord.json').read_text())
config = json.loads(Path('/etc/censorpivot/config.json').read_text())
guard_config = daemon['censorguard']['config']
group = 'censorguard-dsh-default'
original_policy = '/etc/censorguard/policy.d/censorguard-dsh-default.yaml'
events = []
done = threading.Event()
stream = None
reader = None


def command(args, actor=False, allowed_codes=(0,)):
    if actor:
        args = ['runuser', '-u', 'censorpivot', '--', *args]
    result = subprocess.run(args, capture_output=True, text=True, timeout=90)
    assert result.returncode in allowed_codes, (args, result.stdout, result.stderr)
    return json.loads(result.stdout)


def ctl(*args):
    return command(['/usr/bin/censorguardctl', '--config', guard_config, *args, '--json'])


def fs(*args):
    return command([config['censorfs']['command'], '--socket', config['censorfs']['socket'], '--json', *args], True)


def submit(label, calls):
    head = fs('head', 'main')
    batch = dict(request_id=f'acceptance-{label}-{uuid.uuid4()}', session_id=f'acceptance-{label}',
                 trace_id=None, branch='main', expected_generation=head['generation_id'],
                 expected_head_seq=head['head_seq'], guard_scope='caller-domain-is-not-trusted',
                 guard_group=group, calls=calls)
    path = root / f'{label}-batch.json'
    path.write_text(json.dumps(batch))
    result = command(['/usr/local/bin/censorpivot', 'submit', '--file', str(path)], True, (0, 2))
    assert result['kind'] == 'transaction', result
    record = result['data']
    (root / f'{label}.json').write_text(json.dumps(record, indent=2))
    assert record['summary']['guard_scope'] == f"pivot-{record['transaction_id']}", record
    return record, head, path


def call(label, program, args):
    return dict(id=label, program=program, args=args, cwd='.', timeout_ms=5000)


try:
    assert os.geteuid() == 0
    assert daemon['censorfs']['socket'] == config['censorfs']['socket']
    assert daemon['censorguard']['config'] == config['censorguard']['config']
    assert daemon['censorscope']['config'] == config['censorscope']['operator_config']
    assert daemon['censorscope']['socket'] == config['censorscope']['socket']
    doctor = command(['/usr/local/bin/censorpivot', 'doctor'], True)
    assert doctor['kind'] == 'doctor', doctor
    assert all(value == 'ok' for value in doctor['data']['checks'].values()), doctor
    (root / 'doctor.json').write_text(json.dumps(doctor, indent=2))
    secret = root / 'blocked.txt'
    secret.write_text('acceptance-only protected content\n')
    policy = root / 'policy.yaml'
    policy.write_text(Path(original_policy).read_text() +
                      f'\n  - file deny+audit {secret} [read]\n  - exec deny+audit /usr/bin/false\n')
    ctl('policy', 'apply', '--policy', group, '--file', str(policy))
    status = ctl('status')
    assert status['config_path'] == guard_config, status
    (root / 'guard-status.json').write_text(json.dumps(status, indent=2))
    stream = socket.socket(socket.AF_UNIX)
    stream.connect('/run/censorguard/events.sock')
    stream.settimeout(0.2)

    def receive():
        pending = b''
        while not done.is_set():
            try:
                chunk = stream.recv(65536)
            except socket.timeout:
                continue
            except OSError:
                return
            if not chunk:
                return
            pending += chunk
            while b'\n' in pending:
                frame, pending = pending.split(b'\n', 1)
                events.append(json.loads(frame))

    reader = threading.Thread(target=receive, daemon=True)
    reader.start()
    marker = f'acceptance-{uuid.uuid4()}.txt'
    committed, before, path = submit('commit', [
        call('write', '/bin/sh', ['-c', f"id -u; grep ' /workspace ' /proc/self/mountinfo; printf 'accepted\\n' > {marker}"]),
        call('verify', '/usr/bin/cat', [marker]),
    ])
    assert committed['state'] == 'committed', committed
    assert committed['summary']['trace_id'], committed
    assert committed['results'][1]['stdout'] == 'accepted\n', committed
    assert 'fuse' in committed['results'][0]['stdout'], committed
    assert committed['results'][0]['stdout'].splitlines()[0] != '0', committed
    after = fs('head', 'main')
    assert after['head_seq'] == before['head_seq'] + 1, (before, after)
    replay = command(['/usr/local/bin/censorpivot', 'submit', '--file', str(path)], True)
    assert replay['data']['transaction_id'] == committed['transaction_id'], replay
    assert fs('head', 'main') == after
    print('PASS real FUSE View, non-root tools, Scope trace, Commit/CAS and idempotent replay', flush=True)
    for label, program, args, kind in [('file-deny', '/usr/bin/cat', [str(secret)], 1),
                                        ('exec-deny', '/usr/bin/false', [], 2)]:
        denied, before, _ = submit(label, [
            call('private-write', '/bin/sh', ['-c', f"printf private > never-publish-{label}.txt"]),
            call('blocked', program, args),
            call('must-not-run', '/usr/bin/true', []),
        ])
        assert denied['state'] == 'aborted', denied
        assert len(denied['results']) < 3, denied
        assert 'Operation not permitted' in json.dumps(denied), denied
        assert fs('head', 'main') == before
        for _ in range(40):
            matching = [event for event in events if event.get('domain') == denied['summary']['guard_scope']
                        and event.get('kind') == kind and event.get('allowed') is False]
            if matching:
                break
            time.sleep(0.05)
        assert matching, (denied, events[-20:])
        print(f'PASS {label}: actual EPERM, matching audit, Abort, no Head advancement or later tool', flush=True)
    assert not list(Path('/run/censorpivot-cgroups').glob('runner-*.scope'))
    print('PASS batch cgroup descendants cleaned up; shared component configuration verified', flush=True)
    # Read the actual operator-configured database. A trace id and call spans
    # alone do not prove that kernel events were collected and attributed.
    import tomllib
    scope_config = tomllib.loads(Path(daemon['censorscope']['config']).read_text())
    database = sqlite3.connect(f"file:{scope_config['storage']['path']}?mode=ro", uri=True)
    evidence = []
    for label in ['commit', 'file-deny', 'exec-deny']:
        record = json.loads((root / f'{label}.json').read_text())
        trace = record['summary']['trace_id']
        calls = [result['call_id'] for result in record['results']]
        for _ in range(40):
            attributed = dict(database.execute(
                'SELECT call_id, COUNT(*) FROM events WHERE trace_id=? GROUP BY call_id', (trace,)))
            spans = database.execute(
                'SELECT call_id, host_pid, status, ended_at IS NOT NULL FROM call_spans WHERE trace_id=?', (trace,)).fetchall()
            if all(attributed.get(call_id, 0) > 0 for call_id in calls) and len(spans) == len(calls):
                break
            time.sleep(0.05)
        assert all(attributed.get(call_id, 0) > 0 for call_id in calls), (label, attributed)
        assert all(span[3] for span in spans), (label, spans)
        evidence.append(dict(test=label, trace_id=trace, calls=attributed, spans=spans))
    database.close()
    (root / 'scope-evidence.json').write_text(json.dumps(evidence, indent=2))
    print('PASS short-batch kernel events persisted and attributed to every executed tool call', flush=True)
finally:
    done.set()
    if stream:
        stream.close()
    if reader:
        reader.join(timeout=2)
    (root / 'audit.jsonl').write_text(''.join(json.dumps(event) + '\n' for event in events))
    if os.geteuid() == 0:
        ctl('policy', 'apply', '--policy', group, '--file', original_policy)
    print(f'Artifacts: {root}', flush=True)
