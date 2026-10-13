#!/usr/bin/env python3
"""Real Guard/Scope registration and permission regression on a BPF host.

Run after building both components' release binaries and Guard's BPF object:
  sudo -H python3 CensorScope/scripts/test-guard-compatibility.py
Requires BPF LSM, tracefs, root, Node 22+, setpriv and an existing censorguard
OS group. Uses temporary paths; never changes accounts or installed services.
Leaves logs and isolated databases under /tmp/scope-guard-audit-*.
"""
import grp
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import sqlite3
import threading
import time

repo = Path(__file__).resolve().parents[2]
root = Path(tempfile.mkdtemp(prefix='scope-guard-audit-'))
root.chmod(0o755)
print(f'Artifacts: {root}', flush=True)
children = []
logs = []
events = []
audit_stop = threading.Event()
audit_socket = None
audit_thread = None

def command(args, timeout=30):
    process = subprocess.Popen([str(x) for x in args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
        return {'returncode': process.returncode, 'stdout': stdout, 'stderr': stderr}
    finally:
        if process.poll() is None:
            os.killpg(process.pid, 9)
            process.communicate()

def save(name, result):
    (root/f'{name}.json').write_text(json.dumps(result, indent=2))
    if isinstance(result, dict) and 'returncode' in result:
        print(f'{name}: exit={result["returncode"]}', flush=True)
    else:
        print(f'{name}: {len(result)} records', flush=True)
    return result

def start(args, name):
    log_path = root/f'{name}.log'
    log = log_path.open('w')
    logs.append(log)
    process = subprocess.Popen([str(x) for x in args], stdout=log, stderr=log, start_new_session=True)
    children.append(process)
    return process, log_path

def wait_ready(process, log, text):
    for _ in range(400):
        if process.poll() is not None:
            raise RuntimeError(log.read_text())
        if text in log.read_text():
            return
        time.sleep(0.05)
    raise RuntimeError(f'not ready: {log.read_text()}')

try:
    assert os.geteuid() == 0
    for name, source in {
        'guardctl': repo/'CensorGuard/target/release/censorguardctl',
        'scopectl': repo/'CensorScope/target/release/censorscopectl',
        'scoped': repo/'CensorScope/target/release/censorscoped',
    }.items():
        shutil.copy2(source, root/name)
        (root/name).chmod(0o755)
    runtime = root/'guard'
    runtime.mkdir(mode=0o755)
    guard_config = root/'guard.yaml'
    group = grp.getgrnam('censorguard')
    guard_config.write_text(f'''daemon:
  bpf_object: {repo}/CensorGuard/bpf/enforce.bpf.o
  control_socket: {runtime}/ctl.sock
  event_socket: {runtime}/events.sock
  launch_socket: {runtime}/launch.sock
  dsh_socket: {runtime}/dsh.sock
  ui_socket: {runtime}/ui.sock
  state_dir: {root}/guard-state
  socket_group: {group.gr_name}
  spawn_grpc: false
  enable_file: true
  enable_exec: true
  enable_net: true
  audit_file: true
  audit_exec: true
  audit_net: true
rules: []
''')
    daemon, log = start([repo/'CensorGuard/target/release/censorguardd', 'launch', '--config', guard_config], 'guard')
    wait_ready(daemon, log, '[READY]')
    guard = [root/'guardctl', '--config', guard_config]
    audit_socket = socket.socket(socket.AF_UNIX)
    audit_socket.connect(str(runtime/'events.sock'))
    audit_socket.settimeout(0.2)
    def receive():
        pending = b''
        while not audit_stop.is_set():
            try:
                data = audit_socket.recv(65536)
            except socket.timeout:
                continue
            if not data:
                return
            pending += data
            while b'\n' in pending:
                frame, pending = pending.split(b'\n', 1)
                event = json.loads(frame)
                if event['kind'] == 4:
                    events.append(event)
    audit_thread = threading.Thread(target=receive, daemon=True)
    audit_thread.start()

    scope_configs = {}
    for label in ['outside', 'inside']:
        directory = root/label
        directory.mkdir(mode=0o755)
        patch = directory/'patch.toml'
        config = directory/'scope.conf'
        patch.write_text(f'''[daemon]
socket_path = "{directory}/ctl.sock"
pid_file = "{directory}/daemon.pid"
log_path = "{directory}/daemon.log"
[storage]
path = "{directory}/scope.sqlite"
[ebpf]
enabled = "true"
tracked_process_max_entries = 4096
pending_operation_max_entries = 4096
event_ring_buffer_max_bytes = 1048576
[writer]
queue_budget_bytes = 8388608
''')
        result = command([root/'scoped', '--config', config, 'init', '--patch', patch])
        assert result['returncode'] == 0, result
        scope_configs[label] = config
    scoped, scope_log = start([root/'scoped', '--config', scope_configs['outside'], '--level', 'L1', 'run'], 'scope-outside')
    wait_ready(scoped, scope_log, 'daemon listening')
    scope = [root/'scopectl', '--config', scope_configs['outside'], '--json']
    outside_doctor = save('scope-outside-doctor', command([*scope, 'doctor']))
    assert outside_doctor['returncode'] == 0 and 'ebpf' in outside_doctor['stdout'], outside_doctor
    inside, inside_log = start([*guard, 'spawn', '--domain', 'scope-loader', '--', root/'scoped', '--config', scope_configs['inside'], '--level', 'L1', 'run'], 'scope-inside')
    wait_ready(inside, inside_log, 'daemon listening')
    rejected_loader = save('scope-loader-inside-track', command([root/'scopectl', '--config', scope_configs['inside'], '--json', 'track-add', '--root-pid', str(os.getpid()), '--name', 'untracked-test']))
    assert rejected_loader['returncode'] != 0 and 'peer_identity' in rejected_loader['stdout'], rejected_loader
    save('scope-inside-doctor', command([*guard, 'spawn', '--domain', 'scope-client', '--', *scope, 'doctor']))

    workload = root/'workload.py'
    workload.write_text('''import os, subprocess, sys
args = [sys.argv[1], '--config', sys.argv[2], '--json', 'track-add', '--root-pid', str(os.getpid()), '--name', sys.argv[3]]
if len(sys.argv) > 4:
    args += ['--root-pid-namespace', os.readlink('/proc/self/ns/pid')]
result = subprocess.run(args, capture_output=True, text=True)
print(result.stdout, end='')
print(result.stderr, end='', file=sys.stderr)
sys.exit(result.returncode)
''')
    save('scope-outside-track', command(['/usr/bin/python3', workload, root/'scopectl', scope_configs['outside'], 'outside-target']))
    legacy = save('scope-inside-track', command([*guard, 'spawn', '--domain', 'scope-client', '--', '/usr/bin/python3', workload, root/'scopectl', scope_configs['outside'], 'inside-target']))
    assert legacy['returncode'] != 0 and 'Permission denied' in legacy['stderr'], legacy
    fixed = save('scope-inside-track-explicit', command([*guard, 'spawn', '--domain', 'scope-client', '--', '/usr/bin/python3', workload, root/'scopectl', scope_configs['outside'], 'inside-explicit', 'explicit']))
    assert fixed['returncode'] == 0, fixed
    for groups, label in [(['--clear-groups'], 'no-group'), ([f'--groups={group.gr_gid}'], 'with-group')]:
        fixed = save(f'scope-nonroot-{label}', command([*guard, 'spawn', '--domain', 'scope-client', '--', 'setpriv', '--reuid=65534', '--regid=65534', *groups, '/usr/bin/python3', workload, root/'scopectl', scope_configs['outside'], label, 'explicit']))
        assert fixed['returncode'] == 0, fixed
    invalid = save('scope-invalid-namespace', command([*scope, 'track-add', '--root-pid', str(os.getpid()), '--root-pid-namespace', 'pid:[1]']))
    assert invalid['returncode'] != 0, invalid
    foreign = save('scope-foreign-uid', command([*guard, 'spawn', '--domain', 'scope-client', '--', 'setpriv', '--reuid=65534', '--regid=65534', '--clear-groups', *scope, 'track-add', '--root-pid', str(os.getpid()), '--root-pid-namespace', os.readlink('/proc/self/ns/pid')]))
    assert foreign['returncode'] != 0 and 'cannot act for target' in foreign['stdout'], foreign
    bpf_probe = '''import ctypes, os, platform
attr = (ctypes.c_uint32 * 36)()
attr[0:4] = [2, 4, 4, 1]
libc = ctypes.CDLL(None, use_errno=True)
fd = libc.syscall(321 if platform.machine() == 'x86_64' else 280, 0, attr, ctypes.sizeof(attr))
print('bpf map_create fd=%d errno=%d' % (fd, ctypes.get_errno()))
if fd >= 0: os.close(fd)
raise SystemExit(fd < 0)
'''
    outside = save('bpf-outside', command(['/usr/bin/python3', '-c', bpf_probe]))
    inside = save('bpf-inside-root-with-group', command([*guard, 'spawn', '--domain', 'scope-client', '--', 'setpriv', '--reuid=0', '--regid=0', f'--groups={group.gr_gid}', '/usr/bin/python3', '-c', bpf_probe]))
    assert outside['returncode'] == 0 and inside['returncode'] != 0 and 'errno=1' in inside['stdout'], (outside, inside)

    host_copy = root/'host.mjs'
    shutil.copy2(repo/'CensorScope/plugins/censorscope-host/lib/index.mjs', host_copy)
    host_entry = root/'host-entry.mjs'
    host_entry.write_text('''import { readFile } from 'node:fs/promises'
import { setTimeout } from 'node:timers/promises'
import { apply } from './host.mjs'
apply({get() { return { register() {return () => {}} } }, on() {}, effect() {}})
for (let i=0; i<200; i++) {
  try {
    const record = JSON.parse(await readFile(process.env.DSH_HOME+'/.censorscope/trace.json', 'utf8'))
    if (record.rootPid !== process.pid) throw new Error('wrong target')
    console.log(JSON.stringify(record))
    process.exit(0)
  } catch { await setTimeout(50) }
}
throw new Error('host registration failed')
''')
    wrapper = root/'scope-ctl.sh'
    wrapper.write_text(f'#!/bin/sh\nexec "{root}/scopectl" --config "{scope_configs["outside"]}" "$@"\n')
    wrapper.chmod(0o755)
    host = save('scope-real-host-under-guard', command([*guard, 'spawn', '--domain', 'scope-client', '--', 'env', f'DSH_HOME={root}/dsh', f'CENSORSCOPE_CTL={wrapper}', shutil.which('node'), host_entry]))
    assert host['returncode'] == 0, host

    # Compare supplementary-group access without changing /etc/group.
    unprivileged = ['setpriv', '--reuid=65534', '--regid=65534']
    no_group = save('socket-no-group', command([*unprivileged, '--clear-groups', *guard, 'status', '--json']))
    with_group = save('socket-with-group', command([*unprivileged, f'--groups={group.gr_gid}', *guard, 'status', '--json']))
    assert no_group['returncode'] != 0 and with_group['returncode'] == 0
    empty_policy = root/'empty.yaml'
    empty_policy.write_text('rules: []\n')
    admin = save('socket-group-does-not-grant-admin', command([*unprivileged, f'--groups={group.gr_gid}', *guard, 'policy', 'apply', '--policy', 'test', '--file', empty_policy, '--dry-run', '--json']))
    assert admin['returncode'] != 0 and 'requires root' in admin['stdout'], admin
    os.chown(runtime, 0, group.gr_gid)
    runtime.chmod(0o750)
    connect_code = 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); print("connected")'
    no_group = save('parent-no-group', command([*unprivileged, '--clear-groups', '/usr/bin/python3', '-c', connect_code, runtime/'dsh.sock']))
    with_group = save('parent-with-group', command([*unprivileged, f'--groups={group.gr_gid}', '/usr/bin/python3', '-c', connect_code, runtime/'dsh.sock']))
    assert no_group['returncode'] != 0 and with_group['returncode'] == 0
    time.sleep(0.3)
    save('guard-denials', events)
    assert any(event['op'] == 2 and not event['allowed'] for event in events)
    assert any(event['op'] == 4 and not event['allowed'] for event in events)
finally:
    for process in reversed(children):
        if process.poll() is None:
            os.killpg(process.pid, 15)
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, 9)
                process.wait(timeout=5)
    audit_stop.set()
    if audit_thread:
        audit_thread.join(timeout=2)
    if audit_socket:
        audit_socket.close()
    (root/'guard-denials.json').write_text(json.dumps(events, indent=2))
    for log in logs:
        log.close()

# Daemon shutdown commits the pending capture batches before inspecting storage.
with sqlite3.connect(f'file:{root}/outside/scope.sqlite?mode=ro', uri=True) as database:
    rows = database.execute('SELECT traces.display_name, COUNT(events.trace_id) FROM traces LEFT JOIN events USING(trace_id) GROUP BY traces.trace_id').fetchall()
    expected = {'inside-explicit', 'no-group', 'with-group', 'dsh-main'}
    counts = dict(rows)
    assert all(counts.get(label, 0) > 0 for label in expected), counts
    save('captured-events', counts)
print('PASS Guard/Scope registration, UID authorization, OS groups and BPF self-protection', flush=True)
