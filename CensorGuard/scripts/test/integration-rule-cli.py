#!/usr/bin/env python3
"""Global rule persistence and hot application on a host with BPF LSM enabled."""
import errno
import json
import os
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


def probe(directory):
    def blocked(operation):
        try:
            operation()
            return False
        except OSError as error:
            return error.errno in (errno.EPERM, errno.EACCES)

    def execute(program):
        subprocess.run([program], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       timeout=5, check=False)

    def connect():
        with socket.socket() as connection:
            connection.settimeout(1)
            connection.connect(('127.0.0.2', 39443))

    return {
        'file': blocked(lambda: (directory / 'secret').read_text()),
        'group_file': blocked(lambda: (directory / 'group-secret').read_text()),
        'id': blocked(lambda: execute('/usr/bin/id')),
        'true': blocked(lambda: execute('/usr/bin/true')),
        'false': blocked(lambda: execute('/usr/bin/false')),
        'net': blocked(connect),
    }


def read_line(process):
    assert select.select([process.stdout], [], [], 12)[0], 'agent timed out'
    line = process.stdout.readline()
    assert line, f'agent exited: {process.poll()}'
    return json.loads(line)


def ask(process):
    process.stdin.write('probe\n')
    process.stdin.flush()
    return read_line(process)


def agent(directory):
    child = subprocess.Popen([sys.executable, str(SCRIPT), '--child', str(directory)],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        print(json.dumps({'pid': os.getpid()}), flush=True)
        for _ in sys.stdin:
            fresh = subprocess.run([sys.executable, str(SCRIPT), '--probe', str(directory)],
                                   capture_output=True, text=True, check=True, timeout=10)
            print(json.dumps({'root': probe(directory), 'child': ask(child),
                              'fresh': json.loads(fresh.stdout)}), flush=True)
    finally:
        child.terminate()
        child.wait(timeout=5)


def acceptance(directory):
    directory.chmod(0o755)
    for name in ('secret', 'group-secret'):
        (directory / name).write_text('secret')
    config = directory / 'base.yaml'
    settings = {
        'bpf_object': str(REPO / 'bpf/enforce.bpf.o'),
        'control_socket': str(directory / 'ctl.sock'),
        'event_socket': str(directory / 'events.sock'),
        'launch_socket': str(directory / 'launch.sock'),
        'dsh_socket': str(directory / 'dsh.sock'),
        'ui_socket': str(directory / 'ui.sock'),
        'state_dir': str(directory / 'state'),
        'spawn_grpc': False,
        'dns_refresh_seconds': 1,
        'failure_mode': 'permissive',
    }
    config.write_text(json.dumps({'daemon': settings, 'rules': ['exec deny /usr/bin/id']}))
    group = directory / 'group.yaml'
    group.write_text('rules:\n  - exec deny /usr/bin/false\n')
    ctl_binary = directory / 'censorguardctl'
    shutil.copy2(BIN / 'censorguardctl', ctl_binary)
    processes = []
    log = (directory / 'daemon.log').open('w')

    def ctl(*args, success=True, **kwargs):
        result = subprocess.run([str(ctl_binary), '--config', str(config),
                                 *args, '--json'], capture_output=True, text=True,
                                timeout=15, **kwargs)
        assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
        return json.loads(result.stdout) if success else result

    def start():
        daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)],
                                  stdout=log, stderr=log)
        processes.append(daemon)
        for _ in range(300):
            assert daemon.poll() is None, (directory / 'daemon.log').read_text()[-8000:]
            if (directory / 'ctl.sock').exists():
                status = ctl('status')
                if status['daemon_ready']:
                    return daemon
            time.sleep(0.05)
        raise AssertionError('daemon did not become ready')

    def attach(domain):
        process = subprocess.Popen([sys.executable, str(SCRIPT), '--agent', str(directory)],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        processes.append(process)
        identity = read_line(process)
        ctl('attach', '--pid', str(identity['pid']), '--domain', domain)
        return process

    def rpc(method, params):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(12)
            connection.connect(str(directory / 'ctl.sock'))
            connection.sendall((json.dumps({'v': 3, 'request_id': 'rule-test',
                                           'method': method, 'params': params}) + '\n').encode())
            return json.loads(connection.makefile().readline())

    def check(agents, expected):
        for process in agents:
            for owner, actual in ask(process).items():
                assert actual == expected, (owner, actual, expected)

    try:
        daemon = start()
        ctl('policy', 'apply', '--policy', 'worker', '--file', str(group))
        for domain in ('one', 'two'):
            ctl('bind', '--domain', domain, '--policy', 'worker')
        agents = [attach('one'), attach('two')]
        initial = dict(file=False, group_file=False, id=True, true=False, false=True, net=False)
        check(agents, initial)
        before = ctl('status')
        snapshot = rpc('get_policy', {})['result']
        rejected = rpc('apply_policy', {
            'expected_revision': snapshot['revision'],
            'policy_yaml': snapshot['policy_yaml'].replace('exec deny /usr/bin/id', 'exec deny /usr/bin/true'),
        })
        assert not rejected['ok'] and 'cannot change global rules' in rejected['error']['message'], rejected
        original = config.read_bytes()
        ctl('rule', 'append', 'exec deny relative', success=False)
        ctl('rule', 'remove', 'exec deny /missing', success=False)
        assert config.read_bytes() == original
        (directory / 'ctl.sock').chmod(0o666)
        denied = ctl('rule', 'append', 'exec deny /usr/bin/true', success=False, user=65534, group=65534)
        assert 'requires root' in denied.stdout + denied.stderr, denied
        (directory / 'ctl.sock').chmod(0o600)
        assert config.read_bytes() == original
        with socket.socket(socket.AF_UNIX) as connection:
            connection.connect(str(directory / 'ui.sock'))
            connection.sendall(b'{"v":3,"op":"rule_append","rule":"exec deny /usr/bin/true"}\n')
            assert not json.loads(connection.makefile().readline())['ok']
        file_rule = f'file deny {directory}/secret'
        ctl('rule', 'append', file_rule)
        saved = config.read_bytes()
        ctl('rule', 'append', file_rule)
        assert config.read_bytes() == saved
        ctl('rule', 'append', 'exec deny /usr/bin/true')
        ctl('rule', 'append', 'net deny 127.0.0.2')
        ctl('rule', 'remove', 'exec deny /usr/bin/id')
        assert ctl('status')['version'] == before['version']
        check(agents, initial)
        group.write_text(f'rules:\n  - exec deny /usr/bin/false\n  - file deny {directory}/group-secret\n')
        ctl('policy', 'apply', '--policy', 'worker', '--file', str(group))
        initial['group_file'] = True
        time.sleep(1.2)
        check(agents, initial)
        config.chmod(0o640)
        original_metadata = config.stat()
        ctl('rule', 'append', 'exec allow /usr/bin/id')
        saved_metadata = config.stat()
        assert (saved_metadata.st_mode, saved_metadata.st_uid, saved_metadata.st_gid) == (
            original_metadata.st_mode, original_metadata.st_uid, original_metadata.st_gid)
        config.write_text(config.read_text().replace('daemon:\n', 'daemon:\n  audit_exec: true\n'))
        ctl('rule', 'apply')
        applied = ctl('status')
        assert applied['audit_exec'] is False
        assert applied['daemon_boot_id'] == before['daemon_boot_id']
        assert applied['roots'] == before['roots']
        assert applied['bindings'] == before['bindings']
        expected = dict(file=True, group_file=True, id=False, true=True, false=True, net=True)
        check(agents, expected)
        print('PASS accumulated edits, permissions, authorization, isolation and hot apply across two process trees', flush=True)

        saved_config = config.read_text()
        config.write_text(json.dumps({'daemon': settings, 'rules': [
            f'file deny {directory}/entry-{index}' for index in range(257)]}))
        ctl('rule', 'apply', success=False)
        assert ctl('status')['version'] == applied['version']
        check(agents, expected)
        config.write_text(saved_config)

        ctl('rule', 'remove', file_rule)
        ctl('rule', 'remove', 'exec deny /usr/bin/true')
        ctl('rule', 'append', 'exec deny /usr/bin/id')
        check(agents, expected)
        assert ctl('status')['version'] == applied['version']
        for process in agents:
            process.stdin.close()
            process.wait(timeout=10)
        daemon.terminate()
        daemon.wait(timeout=10)
        daemon = start()
        restarted = ctl('status')
        assert restarted['audit_exec'] is True
        assert restarted['version'] == applied['version'] + 1
        assert restarted['daemon_boot_id'] != before['daemon_boot_id']
        assert restarted['bindings'] == before['bindings']
        restored = attach('one')
        expected.update(file=False, true=False, id=True)
        check([restored], expected)
        print('PASS failed publication preserves active rules; unapplied additions and removals load on startup', flush=True)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        log.close()


if __name__ == '__main__':
    if len(sys.argv) > 1:
        directory = Path(sys.argv[2])
        if sys.argv[1] == '--agent':
            agent(directory)
        elif sys.argv[1] == '--probe':
            print(json.dumps(probe(directory)))
        elif sys.argv[1] == '--child':
            for _ in sys.stdin:
                print(json.dumps(probe(directory)), flush=True)
    else:
        with tempfile.TemporaryDirectory(prefix='censorguard-rule-') as temporary:
            acceptance(Path(temporary))
