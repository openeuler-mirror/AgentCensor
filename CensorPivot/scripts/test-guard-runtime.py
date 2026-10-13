#!/usr/bin/env python3
"""Real Pivot runner registration, file/exec enforcement and audit on a BPF host.

Requires root, BPF LSM and mount namespaces; no FUSE or service installation.
Everything uses an isolated /tmp config and private root with /workspace.
Usage: python3 scripts/test-guard-runtime.py [GUARD_ROOT] [PIVOT_BINARY]
"""
import grp
import json
import os
from pathlib import Path
import select
import shutil
import socket
import threading
import subprocess
import sys
import tempfile
import time

PIVOT = Path(__file__).resolve().parents[1]
GUARD = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else PIVOT.parent / 'CensorGuard'
RUNNER = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else PIVOT / 'target/debug/censorpivot'
BIN = GUARD / 'target/release'
root = Path(tempfile.mkdtemp(prefix='pivot-guard-'))
os.chmod(root, 0o755)
config = root / 'guard.yaml'
children = []
logs = []
audit_stream = None
audit_thread = None
audit_done = threading.Event()
events = []
print(f'Artifacts: {root}', flush=True)


def ctl(*args):
    result = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), *args, '--json'],
                            capture_output=True, text=True, timeout=15)
    assert result.returncode == 0, (result.stdout, result.stderr)
    return json.loads(result.stdout)


def line(process):
    ready, _, _ = select.select([process.stdout], [], [], 15)
    assert ready, f'runner output timeout: pid={process.pid}'
    value = process.stdout.readline()
    assert value, f'runner exited: {process.poll()}'
    return json.loads(value)


try:
    assert os.geteuid() == 0, 'requires root on a BPF LSM host'
    secret = root / 'secret'
    secret.write_text('protected\n')
    config.write_text(f'''daemon:
  bpf_object: {GUARD}/bpf/enforce.bpf.o
  control_socket: {root}/ctl.sock
  event_socket: {root}/events.sock
  launch_socket: {root}/launch.sock
  dsh_socket: {root}/dsh.sock
  ui_socket: {root}/ui.sock
  state_dir: {root}/state
  socket_group: {grp.getgrgid(65534).gr_name}
  audit_file: true
  audit_exec: true
rules:
  - file deny+audit {secret}
''')
    log = (root / 'daemon.log').open('w')
    logs.append(log)
    daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)], stdout=log, stderr=log)
    children.append(daemon)
    for _ in range(300):
        assert daemon.poll() is None, (root / 'daemon.log').read_text()
        if '[READY]' in (root / 'daemon.log').read_text():
            status = ctl('status')
            if status['daemon_ready']:
                break
        time.sleep(0.05)
    else:
        raise AssertionError('daemon not ready')
    group = root / 'group.yaml'
    group.write_text('rules:\n  - exec deny+audit /usr/bin/id\n')
    ctl('policy', 'apply', '--policy', 'worker', '--file', str(group))
    audit_stream = socket.socket(socket.AF_UNIX)
    audit_stream.connect(str(root / 'events.sock'))
    audit_stream.settimeout(0.2)
    def read_events():
        pending = b''
        while not audit_done.is_set():
            try:
                data = audit_stream.recv(65536)
            except socket.timeout:
                continue
            except OSError:
                return
            if not data:
                return
            pending += data
            while b'\n' in pending:
                frame, pending = pending.split(b'\n', 1)
                event = json.loads(frame)
                if event.get('domain') == 'pivot-real':
                    events.append(event)
                    with (root / 'audit.jsonl').open('a') as stream:
                        stream.write(json.dumps(event) + '\n')
    audit_thread = threading.Thread(target=read_events, daemon=True)
    audit_thread.start()
    jail = root / 'jail'
    for name in ['usr', 'lib', 'lib64', 'tmp', 'proc', 'workspace']:
        (jail / name).mkdir(parents=True, exist_ok=True)
    os.chmod(jail / 'workspace', 0o777)
    mount_script = root / 'mount-runner.sh'
    mount_script.write_text('''#!/bin/sh
set -eu
jail=$1
shift
mount --make-rprivate /
for name in usr lib lib64 tmp proc; do
  mount --rbind "/$name" "$jail/$name"
done
exec chroot "$jail" /usr/bin/setpriv --reuid=65534 --regid=65534 --clear-groups "$@"
''')
    # The private root binds /tmp, not the source checkout under /root.
    # Stage the selected executable so both debug and release paths are visible.
    staged_runner = root / 'censorpivot'
    shutil.copy2(RUNNER, staged_runner)
    os.chmod(staged_runner, 0o755)
    runner_log = (root / 'runner.log').open('w')
    logs.append(runner_log)
    runner = subprocess.Popen(['unshare', '--mount', '/bin/sh', str(mount_script), str(jail), str(staged_runner), '__batch-runner',
        '--max-output-bytes', '4096', '--guard-socket', str(root / 'launch.sock'), '--guard-domain', 'pivot-real', '--guard-group', 'worker'],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=runner_log, text=True)
    children.append(runner)
    ready = line(runner)
    assert ready['kind'] == 'ready', ready
    status = ctl('status')
    assert any(tree['domain'] == 'pivot-real' and any(node['pid'] == ready['data']['host_pid'] and node['root'] for node in tree['nodes']) for tree in status['trees']), status
    probes = [('/usr/bin/cat', [str(secret)], True), ('/usr/bin/id', [], True), ('/usr/bin/true', [], False)]
    for index, (program, args, denied) in enumerate(probes):
        request = {'session_id': 'pivot-real', 'environment': {}, 'call': {'id': str(index), 'program': program, 'args': args, 'cwd': '', 'timeout_ms': 3000}}
        runner.stdin.write(json.dumps(request) + '\n')
        runner.stdin.flush()
        reply = line(runner)
        if reply['kind'] == 'error':
            assert denied and 'Operation not permitted' in reply['data'], reply
        else:
            assert reply['kind'] == 'result', reply
            assert (reply['data']['exit_code'] != 0) == denied, reply
    runner.stdin.close()
    assert runner.wait(timeout=15) == 0, (root / 'runner.log').read_text()
    for _ in range(80):
        if any(event['kind'] == 1 and not event['allowed'] and (str(secret) in event['detail'] or f"ino={secret.stat().st_ino:016x} secret" == event['detail']) for event in events) and any(event['kind'] == 2 and not event['allowed'] and '/usr/bin/id' in event['detail'] for event in events):
            break
        time.sleep(0.05)
    else:
        raise AssertionError(events)
    print('PASS real non-root runner PID registration, baseline file DENY, group exec DENY, allowed tool and matching audit', flush=True)
finally:
    audit_done.set()
    if audit_stream is not None:
        audit_stream.close()
    if audit_thread is not None:
        audit_thread.join(timeout=2)
    for process in reversed(children):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
    for stream in logs:
        stream.close()
