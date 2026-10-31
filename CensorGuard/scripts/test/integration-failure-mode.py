#!/usr/bin/env python3
"""Exercise isolated BPF instances, rule auditing and failed rename tail calls."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[2]
BIN = REPO / 'target/debug'


class Events:
    def __init__(self, path):
        self.items = []
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.connect(str(path))
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()

    def read(self):
        with self.socket.makefile('r') as source:
            for line in source:
                self.items.append(json.loads(line))

    def close(self):
        self.socket.shutdown(socket.SHUT_RDWR)
        self.socket.close()
        self.thread.join(timeout=3)


def map_id(pid, name):
    for descriptor in (Path('/proc') / str(pid) / 'fdinfo').iterdir():
        try:
            metadata = descriptor.read_text()
        except FileNotFoundError:
            continue
        for line in metadata.splitlines():
            if not line.startswith('map_id:'):
                continue
            identifier = line.split()[1]
            info = json.loads(subprocess.check_output(
                ['bpftool', '-j', 'map', 'show', 'id', identifier], text=True))
            if isinstance(info, list):
                info = info[0]
            if info['name'] == name:
                return identifier
    raise AssertionError(f'map {name} not found in daemon {pid}')


def exercise(mode, enabled):
    with tempfile.TemporaryDirectory(prefix='cg-mode-') as temporary:
        directory = Path(temporary)
        deep = directory / 'deep'
        for _ in range(40):
            deep /= 'd'
        deep.mkdir(parents=True)
        (deep / 'file').write_text('deep')
        for name in ('watched', 'blocked', 'quiet', 'source'):
            (directory / name).write_text(name)
        listeners = [socket.socket(), socket.socket()]
        for listener in listeners:
            listener.bind(('127.0.0.1', 0))
            listener.listen(128)
        ports = [listener.getsockname()[1] for listener in listeners]
        config = directory / 'config.yaml'
        config.write_text(f'''daemon:
  bpf_object: {REPO}/bpf/enforce.bpf.o
  control_socket: {directory}/ctl.sock
  event_socket: {directory}/events.sock
  launch_socket: {directory}/launch.sock
  dsh_socket: {directory}/dsh.sock
  ui_socket: {directory}/ui.sock
  state_dir: {directory}/state
  failure_mode: permissive
  dsh_enabled: false
  self_protection: false
  enable_file: {str(enabled).lower()}
  enable_exec: {str(enabled).lower()}
  enable_net: {str(enabled).lower()}
rules: []
''')

        def ctl(*arguments, stdin=None):
            result = subprocess.run([str(BIN / 'censorguardctl'), '--config',
                                     str(config), *arguments], input=stdin,
                                    text=True, capture_output=True, timeout=15)
            assert result.returncode == 0, (arguments, result.stdout, result.stderr)
            return result.stdout

        def status():
            return json.loads(ctl('status', '--json'))

        def publish(rules):
            ctl('policy', 'apply', '--policy', 'observed', '--stdin',
                stdin='rules:\n' + ''.join(f'  - {rule}\n' for rule in rules)
                if rules else 'rules: []\n')

        def probe(rename=False):
            code = '''
import errno, json, pathlib, socket, subprocess
result = {}
def attempt(name, operation):
    try:
        operation()
        result[name] = True
    except OSError as error:
        assert error.errno in (errno.EPERM, errno.EACCES), (name, error)
        result[name] = False
'''
            if rename:
                code += f"attempt('rename', lambda: pathlib.Path({str(directory / 'source')!r}).rename({str(directory / 'destination')!r}))\n"
            else:
                for name in ('watched', 'blocked', 'quiet'):
                    code += f"attempt({name!r}, lambda: pathlib.Path({str(directory / name)!r}).read_text())\n"
                code += f"attempt('deep', lambda: pathlib.Path({str(deep / 'file')!r}).read_text())\n"
                for name in ('observed', 'blocked', 'quiet'):
                    code += f"attempt('exec-{name}', lambda: subprocess.run(['/usr/bin/true', '--{name}'], check=True))\n"
                code += "attempt('exec-whole', lambda: subprocess.run(['/usr/bin/echo', '--whole'], check=True))\n"
                for index, port in enumerate(ports):
                    code += f"attempt('net-{index}', lambda: socket.create_connection(('127.0.0.1', {port}), timeout=2).close())\n"
            code += "print('RESULT ' + json.dumps(result), flush=True)\n"
            output = ctl('spawn', '--domain', 'sample', '--', sys.executable, '-c', code)
            return json.loads(next(line[7:] for line in output.splitlines()
                                   if line.startswith('RESULT ')))

        with (directory / 'daemon.log').open('w+') as log:
            daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config',
                                       str(config), '--failure-mode', mode],
                                      stdout=log, stderr=log)
            events = None
            try:
                def ready(previous_boot=None):
                    deadline = time.monotonic() + 60
                    last_error = None
                    while time.monotonic() < deadline:
                        if daemon.poll() is not None:
                            log.seek(0)
                            raise AssertionError(log.read())
                        try:
                            current = status()
                            if current['daemon_ready'] and current['daemon_boot_id'] != previous_boot:
                                return current
                        except (AssertionError, json.JSONDecodeError, subprocess.TimeoutExpired) as error:
                            last_error = error
                        time.sleep(0.05)
                    raise AssertionError(f'daemon readiness timed out: {last_error}')

                current = ready()
                assert current['failure_mode'] == mode, current
                assert not current['self_protection'], current
                assert not (directory / 'dsh.sock').exists()
                assert not (directory / 'ui.sock').exists()
                events = Events(directory / 'events.sock')
                assert all(probe().values()), 'empty policy must not restrict operations'
                time.sleep(0.2)
                assert not events.items, events.items

                audit_rules = [f'file allow+audit {directory / "watched"}',
                               'exec allow+audit /usr/bin/true --observed',
                               'exec allow+audit /usr/bin/echo',
                               f'net allow+audit 127.0.0.1:{ports[0]}']
                publish(audit_rules)
                ctl('bind', '--domain', 'sample', '--policy', 'observed')
                checkpoint = len(events.items)
                observed = probe()
                assert observed['deep'] == (mode != 'strict' or not enabled), observed
                assert all(value for key, value in observed.items() if key != 'deep'), observed
                time.sleep(0.2)
                captured = events.items[checkpoint:]
                executions = [event for event in captured if event['kind'] == 2]
                assert executions and all(event['allowed'] and
                                          ('--observed' in event['args'] or '--whole' in event['args'])
                                          for event in executions), captured
                assert any('--whole' in event['args'] for event in executions), captured
                assert any(event['kind'] == 1 and event['allowed'] for event in captured), captured
                assert any(event['kind'] == 3 and event['allowed'] for event in captured), captured
                assert status()['check_failures']['file_walk'] > 0

                if not enabled:
                    publish(audit_rules + ['exec deny /usr/bin/true'])
                    checkpoint = len(events.items)
                    assert all(probe().values())
                    time.sleep(0.2)
                    executions = [event for event in events.items[checkpoint:] if event['kind'] == 2]
                    assert any('--observed' in event['args'] and event['allowed']
                               for event in executions), executions

                publish(audit_rules + [f'file deny {directory / "blocked"}',
                                      'exec deny /usr/bin/true --blocked',
                                      f'net deny 127.0.0.1:{ports[1]}'])
                result = probe()
                for name in ('blocked', 'exec-blocked', 'net-1'):
                    assert result[name] == (not enabled), (name, result)

                identifier = map_id(daemon.pid, 'file_stage_call')
                subprocess.run(['bpftool', 'map', 'delete', 'id', identifier,
                                'key', 'hex', '00', '00', '00', '00'], check=True)
                assert probe(rename=True)['rename'] == (mode != 'strict' or not enabled)
                assert status()['check_failures']['tail_call'] > 0

                publish([])
                time.sleep(0.2)
                checkpoint = len(events.items)
                assert all(probe().values()), 'removing rules must restore empty-policy behavior'
                time.sleep(0.2)
                assert not events.items[checkpoint:], events.items[checkpoint:]
                before = status()['failure_mode']
                ctl('policy', 'apply', '--policy', 'other', '--stdin', stdin='rules: []\n')
                assert status()['failure_mode'] == before
                boot_id = status()['daemon_boot_id']
                events.close()
                events = None
                subprocess.run([str(BIN / 'censorguardd'), 'reload', '--config', str(config)],
                               check=True, timeout=40)
                restarted = ready(boot_id)
                assert restarted['daemon_boot_id'] != boot_id
                assert restarted['failure_mode'] == mode, restarted
                print(f'PASS failure_mode={mode} enable={enabled}', flush=True)
            finally:
                if events:
                    events.close()
                daemon.terminate()
                try:
                    daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()
                for listener in listeners:
                    listener.close()


if __name__ == '__main__':
    assert os.geteuid() == 0, 'run as root on a BPF LSM host'
    for failure_mode in ('permissive', 'strict'):
        for enforcement in (False, True):
            exercise(failure_mode, enforcement)
