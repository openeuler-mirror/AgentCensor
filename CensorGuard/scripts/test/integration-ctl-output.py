#!/usr/bin/env python3
"""Exercise the shipped CLI against isolated Unix sockets, without loading BPF."""
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading

REPO = Path(__file__).resolve().parents[2]
CTL = REPO / 'target/release/censorguardctl'


def main():
    with tempfile.TemporaryDirectory(prefix='cg-ctl-output-') as temporary:
        directory = Path(temporary)
        config = directory / 'base.yaml'
        state = directory / 'state'
        state.mkdir()
        (state / 'current').write_text('1\n')
        bpf = directory / 'enforce.bpf.o'
        bpf.write_bytes(b'fixture')
        config.write_text(f'''daemon:
  control_socket: {directory}/ctl.sock
  event_socket: {directory}/events.sock
  launch_socket: {directory}/launch.sock
  dsh_socket: {directory}/dsh.sock
  ui_socket: {directory}/ui.sock
  state_dir: {state}
  bpf_object: {bpf}
rules: []
''')
        response = dict(ok=True, daemon_ready=True, hooks_healthy=True, daemon_boot_id='fixture-boot',
                        config_path=str(directory / 'daemon-policy.yaml'),
                        tracked=2, roots=[123], version=7, enable_file=True, enable_exec=True,
                        enable_net=False, audit_file=True, audit_exec=False, audit_net=False,
                        domains=[dict(name='fixture', id=1, slot=1, group='worker', roots=1, version=7)],
                        groups=[dict(name='worker', version=7, definition={'rules': ['file deny /private/secret']})],
                        trees=[dict(domain='fixture', domain_id=1, nodes=[dict(pid=123, ppid=1, comm='fixture')])])
        stop = threading.Event()
        listeners = []
        threads = []

        def serve(listener, control):
            while not stop.is_set():
                try:
                    connection, _ = listener.accept()
                except TimeoutError:
                    continue
                with connection:
                    if control:
                        with connection.makefile('rb') as reader:
                            request = json.loads(reader.readline())
                        assert request['op'] == 'status', request
                        connection.sendall((json.dumps(response) + '\n').encode())

        for name in ('ctl', 'events', 'launch', 'dsh', 'ui'):
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(directory / f'{name}.sock'))
            listener.listen()
            listener.settimeout(0.1)
            listeners.append(listener)
            thread = threading.Thread(target=serve, args=(listener, name == 'ctl'), daemon=True)
            thread.start()
            threads.append(thread)

        def ctl(*args):
            return subprocess.run([str(CTL), '--config', str(config), *args],
                                  text=True, capture_output=True, timeout=10)

        try:
            human = ctl('status')
            assert human.returncode == 0, human.stderr
            assert 'CensorGuard | Ready' in human.stdout
            assert '2 processes / 1 root / 1 domain' in human.stdout
            assert 'revision 7 / 1 custom group' in human.stdout
            assert f'Base policy   {response["config_path"]}' in human.stdout
            assert 'Groups        worker' in human.stdout
            assert f'Base policy   {config}' not in human.stdout
            assert '/private/secret' not in human.stdout and 'fixture-boot' not in human.stdout
            assert len(human.stdout.splitlines()) < 15
            raw = ctl('status', '--json')
            assert raw.returncode == 0, raw.stderr
            assert json.loads(raw.stdout)['groups'] == response['groups']
            assert json.loads(raw.stdout)['trees'] == response['trees']
            assert json.loads(raw.stdout)['config_path'] == response['config_path']
            print('PASS concise status and complete JSON data')
            policy_path = response.pop('config_path')
            legacy = ctl('status')
            assert legacy.returncode == 0
            assert 'Not reported (update daemon)' in legacy.stdout
            response['config_path'] = policy_path

            diagnosis = ctl('doctor', '--json')
            payload = json.loads(diagnosis.stdout)
            assert (diagnosis.returncode == 0) == payload['ok']
            compact = ctl('doctor')
            assert compact.returncode == diagnosis.returncode
            assert str(bpf) not in compact.stdout  # passing check details stay hidden
            assert not compact.stderr
            detailed = ctl('doctor', '--verbose')
            assert str(bpf) in detailed.stdout
            assert detailed.returncode == diagnosis.returncode

            (state / 'current').unlink()
            broken = ctl('doctor')
            assert broken.returncode != 0
            assert 'Needs attention' in broken.stdout and '! state:' in broken.stdout
            assert 'persistence errors' in broken.stdout and not broken.stderr
            raw_broken = ctl('doctor', '--json')
            assert raw_broken.returncode != 0
            assert json.loads(raw_broken.stdout)['ok'] is False
            assert any(check['component'] == 'state' and not check['ok']
                       for check in json.loads(raw_broken.stdout)['checks'])
            assert ctl('doctor', '--json', '--verbose').returncode != 0
            assert ctl('status', '--json', '--json').returncode != 0
            print('PASS doctor summary/details/JSON and actionable nonzero failures')
            print('\n' + human.stdout + compact.stdout)
            response['ok'] = False
            response['error'] = 'state unavailable'
            rejected = ctl('status')
            assert rejected.returncode != 0
            assert 'state unavailable' in rejected.stdout and 'Managed' not in rejected.stdout
            assert not rejected.stderr
            print('PASS failed status remains concise and does not invent data')
        finally:
            stop.set()
            for thread in threads:
                thread.join(timeout=1)
            for listener in listeners:
                listener.close()


if __name__ == '__main__':
    main()
