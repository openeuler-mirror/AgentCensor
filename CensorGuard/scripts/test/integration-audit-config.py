#!/usr/bin/env python3
"""Real BPF audit switches, config replacement, policy replacement and teardown.

Uses isolated config/state/sockets and a loopback listener. Never edits the
installed configuration or starts/stops the production systemd service.
"""
import errno
import itertools
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import sys
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[2]
BIN = REPO / 'target/release'


def probe(directory, port):
    directory = Path(directory)
    results = {}
    for name in ('plain', 'forced', 'blocked', 'other'):
        try:
            (directory / name).read_text()
            results[name] = 'allowed'
        except OSError as error:
            assert error.errno in (errno.EPERM, errno.EACCES), error
            results[name] = 'denied'
    subprocess.run(['/usr/bin/true'], check=True)
    with socket.create_connection(('127.0.0.1', int(port)), timeout=3):
        pass
    print(json.dumps(results), flush=True)


def rule_probe(executable, port):
    subprocess.run(['/usr/bin/true', '--forced'], check=True)
    subprocess.run([executable], check=True)
    with socket.create_connection(('127.0.0.1', int(port)), timeout=3):
        pass


class Events:
    def __init__(self, path):
        self.items = []
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.connect(str(path))
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()

    def read(self):
        try:
            with self.socket.makefile('r') as source:
                for line in source:
                    self.items.append(json.loads(line))
        except OSError:
            pass

    def close(self):
        self.socket.shutdown(socket.SHUT_RDWR)
        self.socket.close()
        self.thread.join(timeout=3)


def main():
    assert os.geteuid() == 0, 'run as root on a BPF LSM host'
    # Keep executable rule paths within the compiler's 255-byte token boundary.
    # Long runtime argv and shebangs are covered by integration-exec-arguments.py.
    directory = Path(tempfile.mkdtemp(prefix='cga-'))
    print(f'Artifacts: {directory}', flush=True)
    config = directory / 'base.yaml'
    for name in ('plain', 'forced', 'blocked', 'other'):
        (directory / name).write_text(name + '\n')
    listener = socket.socket()
    listener.bind(('127.0.0.1', 0))
    listener.listen(128)
    port = listener.getsockname()[1]
    forced_listener = socket.socket()
    forced_listener.bind(('127.0.0.1', 0))
    forced_listener.listen(128)
    forced_port = forced_listener.getsockname()[1]
    late_executable = directory / 'late'
    probe_script = directory / 'probe.py'
    probe_script.symlink_to(Path(__file__).resolve())
    daemon = None
    collector = None
    agents = []
    all_events = []

    def write_config(audits, blocked='blocked'):
        content = f'''daemon:
  bpf_object: {REPO}/bpf/enforce.bpf.o
  control_socket: {directory}/ctl.sock
  event_socket: {directory}/events.sock
  launch_socket: {directory}/launch.sock
  dsh_socket: {directory}/dsh.sock
  ui_socket: {directory}/ui.sock
  state_dir: {directory}/state
  audit_file: {str(audits[0]).lower()}
  audit_exec: {str(audits[1]).lower()}
  audit_net: {str(audits[2]).lower()}
rules:
  - file deny {directory / blocked}
  - file allow+audit {directory / 'forced'}
  - exec allow+audit /usr/bin/true --forced
  - exec allow+audit {late_executable}
  - net allow+audit 127.0.0.1:{forced_port}
'''
        # Match editors that save to a new inode and atomically replace the YAML.
        temporary = config.with_suffix('.new')
        temporary.write_text(content)
        temporary.replace(config)

    def ctl(*args):
        if '--json' not in args:
            args = (*args, '--json')
        result = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), *args],
                                text=True, capture_output=True, timeout=12)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return json.loads(result.stdout)

    def status():
        return ctl('status', '--json')

    def daemon_command(action):
        result = subprocess.run([str(BIN / 'censorguardd'), action, '--config', str(config)],
                                text=True, capture_output=True, timeout=40)
        assert result.returncode == 0, (action, result.stdout, result.stderr)

    def reload():
        nonlocal collector
        before = status()
        collector.close()
        all_events.extend(collector.items)
        daemon_command('reload')
        after = status()
        assert after['daemon_boot_id'] != before['daemon_boot_id']
        assert after['config_path'] == str(config.resolve())
        collector = Events(directory / 'events.sock')
        time.sleep(0.1)
        return after

    def run_probe(audits, blocked='blocked', additionally_denied=()):
        checkpoint = len(collector.items)
        agent = subprocess.Popen([str(BIN / 'censorguardctl'), '--config', str(config),
                                  'spawn', '--domain', 'audit-test', '--',
                                  sys.executable, str(probe_script), '--probe',
                                  str(directory), str(port)],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        agents.append(agent)
        stdout, stderr = agent.communicate(timeout=10)
        assert agent.returncode == 0, (stdout, stderr)
        results = json.loads(next(line for line in stdout.splitlines() if line.startswith('{')))
        denied = {blocked, *additionally_denied}
        for name in ('plain', 'forced', 'blocked', 'other'):
            assert results[name] == ('denied' if name in denied else 'allowed'), results
        time.sleep(0.3)
        events = [event for event in collector.items[checkpoint:] if event.get('domain') == 'audit-test']
        def contains(kind, allowed=True, detail=None):
            return any(event['kind'] == kind and event['allowed'] == allowed
                       and (detail is None or detail in event['detail']) for event in events)
        # EXEC must follow audit_exec independently of file/net's switches.
        actual = (contains(1, detail='/plain'), contains(2), contains(3))
        assert actual == audits, {'configured': audits, 'observed': actual, 'events': events}
        for name in denied:
            assert contains(1, False, name), events
        assert contains(1, True, '/forced'), events  # per-rule +audit overrides global off
        print(f'PASS audit file/exec/net={audits}: actual ALLOW={actual}; DENY and rule +audit retained', flush=True)

    def run_rule_probe():
        checkpoint = len(collector.items)
        result = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config),
                                 'spawn', '--domain', 'audit-test', '--', sys.executable,
                                 str(probe_script), '--rule-probe',
                                 str(late_executable), str(forced_port)],
                                text=True, capture_output=True, timeout=10)
        assert result.returncode == 0, (result.stdout, result.stderr)
        time.sleep(0.3)
        events = [event for event in collector.items[checkpoint:]
                  if event.get('domain') == 'audit-test' and event['allowed']]
        assert any(event['kind'] == 2 and event['detail'] == '/usr/bin/true'
                   and '--forced' in event['args'] for event in events), events
        assert any(event['kind'] == 2 and event['detail'] == str(late_executable)
                   for event in events), events
        assert any(event['kind'] == 3 and event['detail'].endswith(f':{forced_port:05}')
                   for event in events), events
        print('PASS EXEC argument/string-fallback and NET rule +audit with all global audit switches off', flush=True)

    try:
        write_config((False, False, False))
        log = (directory / 'daemon.log').open('w')
        daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)],
                                  stdout=log, stderr=log)
        for _ in range(300):
            assert daemon.poll() is None, (directory / 'daemon.log').read_text()
            if (directory / 'ctl.sock').exists():
                try:
                    if status()['daemon_ready']:
                        break
                except (AssertionError, json.JSONDecodeError):
                    pass
            time.sleep(0.05)
        else:
            raise AssertionError('daemon did not become ready')
        collector = Events(directory / 'events.sock')
        time.sleep(0.1)
        # Compile a nonexistent executable by string, then create it after policy loading.
        shutil.copy('/usr/bin/true', late_executable)
        run_rule_probe()
        run_probe((False, False, False))
        # An ordinary group allow must not erase a matching baseline +audit marker.
        plain_group = directory / 'plain-allow.yaml'
        plain_group.write_text(f'''rules:
  - file allow {directory / 'forced'}
  - exec allow /usr/bin/true --forced
  - exec allow {late_executable}
  - net allow 127.0.0.1:{forced_port}
''')
        ctl('policy', 'apply', '--policy', 'plain-allow', '--file', str(plain_group))
        ctl('bind', '--domain', 'audit-test', '--policy', 'plain-allow')
        run_probe((False, False, False))
        run_rule_probe()
        ctl('remove', '--domain', 'audit-test', '--policy', 'plain-allow')
        ctl('policy', 'remove', '--policy', 'plain-allow')
        print('PASS group allow preserves baseline allow+audit with global audit disabled', flush=True)
        for audits in itertools.product((False, True), repeat=3):
            if not any(audits):
                continue
            before = status()
            old_inode = config.stat().st_ino
            write_config(audits)
            assert config.stat().st_ino != old_inode
            unchanged = status()
            assert unchanged['daemon_boot_id'] == before['daemon_boot_id']
            assert tuple(unchanged[key] for key in ('audit_file', 'audit_exec', 'audit_net')) == tuple(
                before[key] for key in ('audit_file', 'audit_exec', 'audit_net'))
            after = reload()
            assert tuple(after[key] for key in ('audit_file', 'audit_exec', 'audit_net')) == audits
            run_probe(audits)

        # Public YAML reread is by path, independent of both its inode and target file inode lookup.
        write_config((True, True, True), blocked='other')
        reload()
        run_probe((True, True, True), blocked='other')
        print('PASS changed config inode and baseline rules apply after reload; disk edits alone leave live state intact', flush=True)

        # Imported group files are content snapshots: edits require an explicit policy apply.
        group = directory / 'group.yaml'
        group.write_text(f'rules:\n  - file deny {directory / "blocked"}\n')
        ctl('policy', 'apply', '--policy', 'worker', '--file', str(group))
        ctl('bind', '--domain', 'audit-test', '--policy', 'worker')
        run_probe((True, True, True), blocked='other', additionally_denied=('blocked',))
        replacement = directory / 'group.new'
        replacement.write_text('rules: []\n')
        replacement.replace(group)
        definition = next(item for item in status()['groups'] if item['name'] == 'worker')['definition']
        assert definition['rules'] == [f'file deny {directory / "blocked"}']
        run_probe((True, True, True), blocked='other', additionally_denied=('blocked',))
        ctl('policy', 'apply', '--policy', 'worker', '--file', str(group))
        run_probe((True, True, True), blocked='other')
        print('PASS replaced group YAML is reread by policy apply; stale deny is removed', flush=True)

        old_boot = status()['daemon_boot_id']
        collector.close()
        all_events.extend(collector.items)
        collector = None
        daemon_command('stop')
        assert daemon.wait(timeout=5) == 0
        for endpoint in ('ctl.sock', 'events.sock', 'launch.sock', 'dsh.sock', 'ui.sock'):
            assert not (directory / endpoint).exists(), endpoint
        # Reusing paths/state proves the prior instance released ownership and BPF handles.
        daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)], stdout=log, stderr=log)
        for _ in range(300):
            assert daemon.poll() is None, (directory / 'daemon.log').read_text()
            try:
                current = status()
                if current['daemon_ready'] and current['daemon_boot_id'] != old_boot:
                    break
            except (AssertionError, json.JSONDecodeError):
                pass
            time.sleep(0.05)
        else:
            raise AssertionError('relaunch failed')
        assert all(current[key] for key in ('audit_file', 'audit_exec', 'audit_net'))
        daemon_command('stop')
        assert daemon.wait(timeout=5) == 0
        print('PASS stop exits the process and releases endpoints; relaunch restores new settings', flush=True)
    finally:
        if collector:
            collector.close()
            all_events.extend(collector.items)
        for process in [*agents, daemon]:
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        listener.close()
        forced_listener.close()
        (directory / 'events.json').write_text(json.dumps(all_events, indent=2))


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--probe':
        probe(sys.argv[2], sys.argv[3])
    elif len(sys.argv) > 1 and sys.argv[1] == '--rule-probe':
        rule_probe(sys.argv[2], sys.argv[3])
    else:
        main()
