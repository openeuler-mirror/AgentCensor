#!/usr/bin/env python3
"""Real BPF acceptance for baseline overlays and all three agent entry paths.

Build Rust/BPF and the DSH plugin first. Run as root on a BPF LSM host:
  python3 scripts/test/integration-policy-entry.py
Each run uses an isolated /tmp config/state and stops all processes it starts.
"""
import errno
import json
import os
import pty
from pathlib import Path
import select
import shutil
import socket
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
SCRIPT = Path(__file__).resolve()
BIN = REPO / 'target/debug'


def rpc(path, method, params):
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(8)
        conn.connect(str(path))
        conn.sendall((json.dumps({'v': 2, 'request_id': 'entry-test',
                                 'method': method, 'params': params}) + '\n').encode())
        return json.loads(conn.makefile().readline())


def denied(operation):
    try:
        operation()
        return False
    except OSError as error:
        return error.errno in (errno.EPERM, errno.EACCES)


def probe(directory):
    def read(name):
        with (directory / name).open() as source:
            source.read()

    def execute(*argv):
        subprocess.run(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       timeout=5, check=False)

    def connect(family, host):
        with socket.socket(family) as conn:
            conn.settimeout(1)
            conn.connect((host, 39443))

    return {
        'base_file': denied(lambda: read('base-secret')),
        'group_file': denied(lambda: read('group-secret')),
        'extra_file': denied(lambda: read('extra-secret')),
        'base_exec': denied(lambda: execute('/usr/bin/id')),
        'group_exec': denied(lambda: execute('/usr/bin/true')),
        'extra_exec': denied(lambda: execute('/usr/bin/false')),
        'group_exec_args': denied(lambda: execute('/usr/bin/printf', 'block', 'extra')),
        'other_exec_args': denied(lambda: execute('/usr/bin/printf', 'other')),
        'group_exec_fallback': denied(lambda: execute(str(directory / 'future-cmd'))),
        'base_ipv4': denied(lambda: connect(socket.AF_INET, '127.0.0.2')),
        'group_ipv4': denied(lambda: connect(socket.AF_INET, '127.0.0.3')),
        'extra_ipv4': denied(lambda: connect(socket.AF_INET, '127.0.0.4')),
        'base_ipv6': denied(lambda: connect(socket.AF_INET6, '::1')),
        'group_ipv6': denied(lambda: connect(socket.AF_INET6, '::2')),
        'extra_ipv6': denied(lambda: connect(socket.AF_INET6, '::3')),
    }


def line(process):
    ready, _, _ = select.select([process.stdout], [], [], 12)
    assert ready, f'probe timed out, pid={process.pid}'
    result = process.stdout.readline()
    if result.startswith('tracking pid='):
        result = process.stdout.readline()
    assert result, f'probe exited, pid={process.pid}, rc={process.poll()}'
    return json.loads(result)


def ask(process, command):
    process.stdin.write(command + '\n')
    process.stdin.flush()
    return line(process)


def agent(directory, mode):
    # This child exists before attach/register/attach_self. Another is forked at every probe.
    child = subprocess.Popen([sys.executable, str(SCRIPT), '--child', str(directory)],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        attachment = None
        if mode == 'register':
            attachment = rpc(directory / 'launch.sock', 'register_self',
                             {'scope': 'registered', 'group': 'worker'})
        elif mode == 'dsh':
            attachment = rpc(directory / 'dsh.sock', 'attach_self',
                             {'policy_group': 'worker', 'instance_hint': 'entry', 'seed': True})
        if attachment is not None:
            assert attachment['ok'], attachment
        print(json.dumps({'pid': os.getpid(), 'child': child.pid, 'attachment': attachment}),
              flush=True)
        for command in sys.stdin:
            if command.strip() == 'quit':
                break
            if command.strip() == 'status':
                print(json.dumps(rpc(directory / 'dsh.sock', 'status_self', {})), flush=True)
                continue
            child_result = ask(child, 'probe')
            new_child = subprocess.run([sys.executable, str(SCRIPT), '--probe', str(directory)],
                                       capture_output=True, text=True, timeout=10, check=True)
            print(json.dumps({'root': probe(directory), 'existing_child': child_result,
                              'new_child': json.loads(new_child.stdout)}), flush=True)
    finally:
        child.terminate()
        child.wait(timeout=5)


def assert_layers(result, group=True, extra=False, base=True):
    for owner, probes in result.items():
        for name, actual in probes.items():
            expected = ((base and name.startswith('base_')) or
                        (group and name.startswith('group_')) or
                        (extra and name.startswith('extra_')))
            assert actual == expected, f'{owner}/{name}: denied={actual}, expected={expected}'


def acceptance():
    assert os.geteuid() == 0, 'run as root on a BPF LSM host'
    # Exec rule tokens are limited to 255 bytes; keep the fallback executable path short.
    directory = Path(tempfile.mkdtemp(prefix='cg-e-'))
    print(f'Artifacts: {directory}', flush=True)
    processes = []
    daemon = None
    daemon_log = (directory / 'daemon.log').open('w')
    grpc_log = (directory / 'grpc.log').open('w')
    config = directory / 'base.yaml'
    for name in ['base-secret', 'group-secret', 'extra-secret']:
        (directory / name).write_text('secret\n')
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        grpc_port = listener.getsockname()[1]
    config.write_text(f'''daemon:
  bpf_object: {REPO}/bpf/enforce.bpf.o
  control_socket: {directory}/ctl.sock
  event_socket: {directory}/events.sock
  launch_socket: {directory}/launch.sock
  dsh_socket: {directory}/dsh.sock
  ui_socket: {directory}/ui.sock
  state_dir: {directory}/state
  dsh_self_groups: [worker]
  grpc_listen: 127.0.0.1:{grpc_port}
rules:
  - file deny {directory}/base-secret
  - exec deny /usr/bin/id
  - exec allow /usr/bin/true
  - exec allow {directory}/future-cmd
  - net deny 127.0.0.2/32
  - net deny ::1/128
''')
    group = f'''rules:
  - file allow {directory}/base-secret
  - file deny {directory}/group-secret
  - exec allow /usr/bin/id
  - exec deny /usr/bin/true
  - exec deny /usr/bin/printf block
  - exec deny {directory}/future-cmd
  - net allow 127.0.0.2:39443
  - net allow [::1]:39443
  - net deny 127.0.0.3/32
  - net deny ::2/128
'''
    group_file = directory / 'worker.yaml'
    group_file.write_text(group)
    # Keep the interpreter's first three argv tokens within the exec capture limit.
    agent_script = directory / 'p.py'
    shutil.copyfile(SCRIPT, agent_script)

    def ctl(*args, success=True):
        if args and args[0] in ('status', 'doctor'):
            args = (*args, '--json')
        result = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), *args],
                                capture_output=True, text=True, timeout=12)
        assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
        return json.loads(result.stdout) if success else result

    def start_daemon():
        process = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)],
                                   stdout=daemon_log, stderr=daemon_log)
        processes.append(process)
        for _ in range(300):
            assert process.poll() is None, (directory / 'daemon.log').read_text()[-8000:]
            if (directory / 'ctl.sock').exists():
                ctl('status')
                return process
            time.sleep(0.05)
        raise AssertionError('daemon did not become ready')

    def start_agent(mode, spawn=False):
        argv = [sys.executable, str(agent_script), '--agent', str(directory), mode]
        if spawn:
            argv = [str(BIN / 'censorguardctl'), '--config', str(config), 'spawn',
                    '--domain', 'spawned', '--', *argv]
        process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   text=True)
        processes.append(process)
        return process, line(process)

    def grpc(method, body, success=True):
        result = subprocess.run(['node', str(REPO / 'scripts/censorguard-grpc-call.mjs'),
                                 f'127.0.0.1:{grpc_port}', method, json.dumps(body)],
                                capture_output=True, text=True, timeout=12)
        assert (result.returncode == 0) == success, (method, result.stdout, result.stderr)
        return json.loads(result.stdout) if success else result

    try:
        daemon = start_daemon()
        # The untracked agent has no denies; launching did not control unrelated processes.
        attached, identity = start_agent('attach')
        initial = ask(attached, 'probe')
        assert not any(any(probes.values()) for probes in initial.values()), initial
        implicit, _ = start_agent('attach')
        ctl('attach', '--pid', str(implicit.pid), '--domain', 'implicit')
        ctl('policy', 'apply', '--policy', 'worker', '--file', str(group_file), '--dry-run')
        assert ctl('status')['version'] == 1
        ctl('policy', 'apply', '--policy', 'worker', '--file', str(group_file))
        for domain in ['spawned', 'attached', 'registered']:
            ctl('bind', '--domain', domain, '--policy', 'worker')
        # This inode did not exist during compilation: exercise string exec fallback.
        shutil.copyfile('/usr/bin/true', directory / 'future-cmd')
        (directory / 'future-cmd').chmod(0o755)
        assert_layers(ask(implicit, 'probe'), group=False)
        implicit.stdin.write('quit\n')
        implicit.stdin.flush()
        implicit.wait(timeout=8)
        print('PASS launch baseline, dry-run, implicit domain preservation', flush=True)

        spawned, spawn_id = start_agent('spawn', spawn=True)
        assert_layers(ask(spawned, 'probe'))
        print('PASS pre-exec spawn root and both child generations', flush=True)
        result = ctl('attach', '--pid', str(identity['pid']), '--domain', 'attached')
        assert result['seeded'] >= 1, result
        assert_layers(ask(attached, 'probe'))
        print('PASS attach existing root/child and future fork', flush=True)
        registered, registered_id = start_agent('register')
        assert registered_id['attachment']['result']['response']['seeded'] >= 1
        assert_layers(ask(registered, 'probe'))
        print('PASS SO_PEERCRED register_self with existing child', flush=True)
        # A nonmember cannot override a published binding.
        ctl('policy', 'apply', '--policy', 'other', '--file', str(group_file))
        ctl('bind', '--domain', 'conflict', '--policy', 'worker')
        conflict = rpc(directory / 'launch.sock', 'register_self', {'scope': 'conflict', 'group': 'other'})
        assert not conflict['ok'], conflict
        outside = rpc(directory / 'dsh.sock', 'attach_self', {'policy_group': '__base__'})
        assert not outside['ok'], outside
        dsh, dsh_id = start_agent('dsh')
        dsh_domain = dsh_id['attachment']['result']['response']['domain']['name']
        assert_layers(ask(dsh, 'probe'))
        heartbeat = ask(dsh, 'status')
        assert heartbeat['result']['response']['domains'][0]['name'] == dsh_domain, heartbeat
        print('PASS DSH self-attachment, heartbeat and whitelist rejection', flush=True)

        adapter = subprocess.Popen([str(BIN / 'censorguard-grpc'), '--config', str(config)],
                                   stdout=grpc_log, stderr=grpc_log)
        processes.append(adapter)
        assert grpc('GetPolicy', {'name': '__base__'})['rules']
        grpc('ApplyPolicy', {'name': '__base__', 'policyYaml': 'rules: []\n'}, success=False)
        grpc('Track', {'pid': identity['pid'], 'domain': 'attached'}, success=False)
        grpc('ApplyPolicy', {'name': 'worker', 'policyYaml': 'rules: []\n'})
        for process in [spawned, attached, registered, dsh]:
            assert_layers(ask(process, 'probe'), group=False)
        print('PASS gRPC group update clears group denies and preserves base in all modes', flush=True)
        grpc('ApplyPolicy', {'name': 'worker', 'policyYaml': group})
        for process in [spawned, attached, registered, dsh]:
            assert_layers(ask(process, 'probe'))
        ctl('remove', '--domain', dsh_domain, '--policy', 'worker')
        assert_layers(ask(dsh, 'probe'), group=False)
        ctl('bind', '--domain', dsh_domain, '--policy', 'worker')
        assert_layers(ask(dsh, 'probe'))
        print('PASS bind/remove live DSH domain through canonical policy path', flush=True)
        extra_file = directory / 'extra.yaml'
        extra_file.write_text(f'''rules:
  - file deny {directory}/extra-secret
  - exec deny /usr/bin/false
  - net allow 127.0.0.3:39443
  - net allow [::2]:39443
  - net deny 127.0.0.4/32
  - net deny ::3/128
''')
        ctl('policy', 'apply', '--policy', 'extra', '--file', str(extra_file))
        for domain in [dsh_domain, 'attached']:
            ctl('bind', '--domain', domain, '--policy', 'extra')
        ctl('bind', '--domain', dsh_domain, '--policy', 'extra')
        for process in [dsh, attached]:
            assert_layers(ask(process, 'probe'), extra=True)
        status = ctl('status')
        membership = next(d for d in status['domains'] if d['name'] == dsh_domain)
        assert membership['policies'] == ['extra', 'worker'], membership
        ctl('policy', 'remove', '--policy', 'extra', success=False)
        ctl('remove', '--domain', dsh_domain, '--policy', 'worker')
        assert_layers(ask(dsh, 'probe'), group=False, extra=True)
        ctl('bind', '--domain', dsh_domain, '--policy', 'worker')
        for domain in [dsh_domain, 'attached']:
            ctl('remove', '--domain', domain, '--policy', 'extra')
        ctl('policy', 'remove', '--policy', 'extra')
        assert_layers(ask(dsh, 'probe'))
        assert_layers(ask(attached, 'probe'))
        print('PASS multi-group union, additive/idempotent binding, selective removal and guarded deletion', flush=True)

        sibling, sibling_id = start_agent('attach')
        ctl('attach', '--pid', str(sibling_id['pid']), '--domain', 'attached')
        ctl('unattach', '--pid', str(identity['pid']))
        assert_layers(ask(attached, 'probe'), group=False, base=False)
        assert_layers(ask(sibling, 'probe'))
        ctl('unattach', '--pid', str(sibling_id['pid']))
        assert_layers(ask(sibling, 'probe'), group=False, base=False)
        sibling.stdin.write('quit\n')
        sibling.stdin.flush()
        sibling.wait(timeout=10)
        ctl('attach', '--pid', str(identity['pid']), '--domain', 'attached')
        assert_layers(ask(attached, 'probe'))
        print('PASS unattach detaches descendants and preserves independently registered roots', flush=True)

        ctl('doctor')
        grpc('SetSwitches', {'enableFile': False}, success=False)
        ctl('set', '--enable-file', 'off', success=False)
        print('PASS doctor diagnostics and rejection of runtime switch writes', flush=True)
        trees = ctl('status')['trees']
        assert any(len(tree['nodes']) >= 2 for tree in trees), trees
        for process in [spawned, attached, registered, dsh]:
            process.stdin.write('quit\n')
            process.stdin.flush()
            process.wait(timeout=10)
        revision = ctl('status')['version']
        old_boot = ctl('status')['daemon_boot_id']
        original = config.read_text()
        config.write_text(original.replace('  - exec deny /usr/bin/id', '  - exec deny /usr/bin/whoami')
                          .replace('  dsh_self_groups: [worker]', '  audit_exec: true\n  dsh_self_groups: [worker]'))
        # Applying full policy over raw RPC must never mutate the running base.
        rejected = rpc(directory / 'ctl.sock', 'apply_policy', {'policy_yaml': config.read_text()})
        assert not rejected['ok'], rejected
        assert ctl('status')['daemon_boot_id'] == old_boot
        bad_config = config.read_text()
        config.write_text('daemon:\n  dns_refresh_seconds: 0\nrules: []\n')
        os.kill(daemon.pid, 1)  # SIGHUP rejects invalid input without dropping enforcement.
        time.sleep(0.3)
        assert daemon.poll() is None
        live = rpc(directory / 'ctl.sock', 'health', {})
        assert live['ok'] and live['result']['response']['daemon_boot_id'] == old_boot, live
        config.write_text(bad_config)
        restarted = subprocess.run([str(BIN / 'censorguardd'), 'reload', '--config', str(config)],
                                   capture_output=True, text=True, timeout=12)
        assert restarted.returncode == 0, (restarted.stdout, restarted.stderr)
        for _ in range(300):
            assert daemon.poll() is None, (directory / 'daemon.log').read_text()[-8000:]
            try:
                status = ctl('status')
                if status['daemon_boot_id'] != old_boot:
                    break
            except (AssertionError, json.JSONDecodeError):
                pass
            time.sleep(0.05)
        else:
            raise AssertionError('daemon reload did not produce a new boot')
        assert status['audit_exec'] is True, status
        assert status['version'] == revision + 1, status
        ctl('doctor')
        # Inspect the freshly loaded baseline; reset id deny for the existing probe matrix.
        blocked = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), 'spawn',
                                  '--domain', 'spawned', '--', '/usr/bin/whoami'], capture_output=True, timeout=10)
        assert blocked.returncode != 0, blocked
        config.write_text(original)
        os.kill(daemon.pid, 1)
        new_boot = status['daemon_boot_id']
        for _ in range(300):
            try:
                if ctl('status')['daemon_boot_id'] != new_boot:
                    break
            except (AssertionError, json.JSONDecodeError):
                pass
            time.sleep(0.05)
        else:
            raise AssertionError('SIGHUP did not restart daemon')
        print('PASS immutable base RPC rejection, invalid restart preservation, daemon reload/SIGHUP and fresh settings', flush=True)
        restored, _ = start_agent('spawn', spawn=True)
        assert_layers(ask(restored, 'probe'))
        restored.stdin.write('quit\n')
        restored.stdin.flush()
        restored.wait(timeout=10)
        print('PASS restart restores groups and domain bindings with current baseline', flush=True)
        # A real terminal keeps bash interactive, allowing multiple commands in one controlled tree.
        master, slave = pty.openpty()
        shell = subprocess.Popen([str(BIN / 'censorguardctl'), '--config', str(config), 'spawn',
                                  '--domain', 'spawned', '--', 'bash', '--noprofile', '--norc', '-i'],
                                 stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        processes.append(shell)
        os.close(slave)
        try:
            os.write(master, b'/usr/bin/id; echo FIRST:$?\n/usr/bin/id; echo SECOND:$?\nexit\n')
            output = bytearray()
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.2)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError:
                        break
                    if not chunk:
                        break
                    output.extend(chunk)
                elif shell.poll() is not None:
                    break
            shell.wait(timeout=5)
            assert b'FIRST:126' in output and b'SECOND:126' in output, output.decode(errors='replace')
        finally:
            os.close(master)
        print('PASS interactive spawn bash supports repeated interception tests', flush=True)

        for args in [('reload', '--policy', 'worker', '--stdin'),
                     ('attach', '--pid', '12', '--policy', 'worker'),
                     ('attach', '--pid', '12', '--seed'),
                     ('spawn', '--policy', 'worker', '--', '/usr/bin/true')]:
            ctl(*args, success=False)
        obsolete = subprocess.run([str(BIN / 'censorguardd'), '--config', str(config)],
                                  capture_output=True, timeout=5)
        assert obsolete.returncode != 0
        print('PASS removed CLI options reject; all policy entry checks passed', flush=True)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        daemon_log.close()
        grpc_log.close()


if __name__ == '__main__':
    if len(sys.argv) == 1:
        acceptance()
    elif sys.argv[1] == '--agent':
        agent(Path(sys.argv[2]), sys.argv[3])
    elif sys.argv[1] == '--child':
        for command in sys.stdin:
            print(json.dumps(probe(Path(sys.argv[2]))), flush=True)
    elif sys.argv[1] == '--probe':
        print(json.dumps(probe(Path(sys.argv[2]))), flush=True)
    else:
        raise SystemExit('unknown probe mode')
