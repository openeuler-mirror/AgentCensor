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
                        policy_snapshot_path=str(state / 'revisions/7.yaml'),
                        tracked=2, roots=[123], version=7, enable_file=True, enable_exec=True,
                        enable_net=False, audit_file=True, audit_exec=False, audit_net=False,
                        domains=[dict(name='fixture', id=1, slot=1, group='worker', roots=1, version=7)],
                        bindings=[dict(name='idle', policies=['extra', 'worker']), dict(name='base-idle', policies=[])],
                        groups=[dict(name='worker', version=7, domains=['fixture', 'idle'],
                                     source=dict(kind='file', path='/policies/worker.yaml'),
                                     definition={'rules': ['file deny /private/secret']}),
                                dict(name='legacy', version=7, definition={}),
                                dict(name='extra', version=7, domains=['idle'], source=dict(kind='api'), definition={})],
                        trees=[dict(domain='fixture', domain_id=1, nodes=[dict(pid=123, ppid=1, comm='fixture', root=True)])])
        mutation = dict(ok=True, daemon_ready=True, version=8, active_bank=1, pid_pending_capacity=1024)
        requests = []
        audit_events = []
        stop = threading.Event()
        listeners = []
        threads = []

        def serve(listener, control, events):
            while not stop.is_set():
                try:
                    connection, _ = listener.accept()
                except TimeoutError:
                    continue
                with connection:
                    if control:
                        with connection.makefile('rb') as reader:
                            request = json.loads(reader.readline())
                        requests.append(request)
                        reply = response if request['op'] == 'status' else mutation
                        connection.sendall((json.dumps(reply) + '\n').encode())
                    elif events:
                        try:
                            connection.sendall(''.join(json.dumps(event) + '\n' for event in audit_events).encode())
                        except BrokenPipeError:
                            pass  # doctor checks socket reachability, then closes.

        for name in ('ctl', 'events', 'launch', 'dsh', 'ui'):
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(directory / f'{name}.sock'))
            listener.listen()
            listener.settimeout(0.1)
            listeners.append(listener)
            thread = threading.Thread(target=serve, args=(listener, name == 'ctl', name == 'events'), daemon=True)
            thread.start()
            threads.append(thread)

        def ctl(*args, stdin=None):
            return subprocess.run([str(CTL), '--config', str(config), *args],
                                  input=stdin, text=True, capture_output=True, timeout=10)

        try:
            human = ctl('status')
            assert human.returncode == 0, human.stderr
            assert 'CensorGuard | Ready' in human.stdout
            assert '2 processes / 1 root' in human.stdout
            assert 'revision 7 / 3 custom groups' in human.stdout
            assert f'Base policy   {response["config_path"]}' in human.stdout
            assert 'fixture [1 root; PID 123]' in human.stdout
            assert 'idle [configured; no processes]' in human.stdout
            assert f'base-idle [configured; no processes]\n  └─ base  {response["config_path"]} (inherited)' in human.stdout
            assert 'Domains (' not in human.stdout
            assert f'fixture [1 root; PID 123]\n  ├─ base  {response["config_path"]} (inherited)\n  └─ worker  /policies/worker.yaml' in human.stdout
            assert f'idle [configured; no processes]\n  ├─ base  {response["config_path"]} (inherited)\n  ├─ extra  {response["policy_snapshot_path"]} (from API/Web)\n  └─ worker  /policies/worker.yaml' in human.stdout
            assert 'worker  /policies/worker.yaml' in human.stdout
            assert f'legacy  {response["policy_snapshot_path"]} (original source unknown)' in human.stdout
            assert f'extra  {response["policy_snapshot_path"]} (from API/Web)' in human.stdout
            assert f'Base policy   {config}' not in human.stdout
            assert '/private/secret' not in human.stdout and 'fixture-boot' not in human.stdout
            assert len(human.stdout.splitlines()) < 34
            raw = ctl('status', '--json')
            assert raw.returncode == 0, raw.stderr
            assert json.loads(raw.stdout)['groups'] == response['groups']
            assert json.loads(raw.stdout)['trees'] == response['trees']
            assert json.loads(raw.stdout)['config_path'] == response['config_path']
            assert json.loads(raw.stdout)['bindings'] == response['bindings']
            response['groups'][2]['source'] = dict(kind='stdin')
            assert f'extra  {response["policy_snapshot_path"]} (from stdin)' in ctl('status').stdout
            response['groups'][2]['source'] = dict(kind='api')
            print('PASS concise status and complete JSON data')
            policy_path = response.pop('config_path')
            legacy = ctl('status')
            assert legacy.returncode == 0
            assert 'Not reported (update daemon)' in legacy.stdout
            response['config_path'] = policy_path

            group_file = directory / 'worker.yaml'
            group_file.write_text('rules: []\n')
            group_alias = directory / 'worker-link.yaml'
            group_alias.symlink_to(group_file)
            applied = ctl('policy', 'apply', '--policy', 'worker', '--file', str(group_alias))
            assert applied.returncode == 0, applied.stderr
            assert 'Policy applied' in applied.stdout and 'Revision      8' in applied.stdout
            assert str(group_file.resolve()) in applied.stdout
            assert 'active_bank' not in applied.stdout and 'pid_pending_capacity' not in applied.stdout
            assert requests[-1]['policy_source'] == dict(kind='file', path=str(group_file.resolve()))
            assert requests[-1]['policy_yaml'] == group_file.read_text() and 'policy_file' not in requests[-1]
            validated = ctl('policy', 'apply', '--policy', 'worker', '--stdin', '--dry-run', stdin='rules: []\n')
            assert validated.returncode == 0 and 'no changes applied' in validated.stdout
            assert 'Revision' not in validated.stdout and 'from stdin' in validated.stdout
            assert requests[-1]['dry_run'] is True and requests[-1]['policy_source'] == dict(kind='stdin')
            for args, label in [
                (('bind', '--domain', 'fixture', '--policy', 'worker'), 'Policy bound'),
                (('remove', '--domain', 'fixture', '--policy', 'worker'), 'Policy unbound'),
                (('policy', 'remove', '--policy', 'worker'), 'Policy removed'),
                (('attach', '--pid', '123', '--domain', 'fixture'), 'Process tree attached'),
                (('unattach', '--pid', '123'), 'Process tree released'),
            ]:
                compact_operation = ctl(*args)
                assert compact_operation.returncode == 0 and label in compact_operation.stdout
                assert 'active_bank' not in compact_operation.stdout and not compact_operation.stderr
                complete_operation = ctl(*args, '--json')
                assert complete_operation.returncode == 0 and json.loads(complete_operation.stdout)['active_bank'] == 1
            mutation['ok'] = False
            mutation['error'] = 'worker is still bound to a domain'
            failed = ctl('policy', 'remove', '--policy', 'worker')
            assert failed.returncode != 0 and mutation['error'] in failed.stdout and not failed.stderr
            failed_json = ctl('policy', 'remove', '--policy', 'worker', '--json')
            assert failed_json.returncode != 0 and json.loads(failed_json.stdout)['ok'] is False
            mutation['ok'] = True
            mutation.pop('error')
            assert ctl('bind', '--domain', 'fixture', '--policy', 'worker', '--json', '--json').returncode != 0
            print('PASS concise mutations, canonical file/stdin sources, dry-run, complete JSON and nonzero errors')

            for kind, allowed, pid, domain, detail in [
                (2, True, 123, 'dsh-web', '/usr/bin/cat'),
                (1, False, 123, 'dsh-web', '/private/secret'),
                (2, False, 124, 'dsh-web', '/usr/bin/echo'),
                (1, False, 125, 'other-domain', '/private/other'),
                (3, False, 123, 'dsh-web', '127.0.0.1:443'),
                (4, False, 123, 'dsh-web', 'kill'),
                (2, True, 126, 'other-domain', '/usr/bin/awk'),
            ]:
                audit_events.append(dict(ts='', kind=kind, op=0, pid=pid, tgid=pid,
                                         allowed=allowed, domain=domain, comm='fixture', detail=detail))
            audit_events[-1]['args'] = ['first\nsecond', '\x1b[31m']
            audit_events[1].update(dropped_before=3, sequence=42)

            def rows(result):
                assert result.returncode == 0, result.stderr
                return [line.split(maxsplit=6) for line in result.stdout.splitlines()
                        if line.startswith(('FILE ', 'EXEC ', 'NET ', 'GUARD '))]

            all_rows = rows(ctl('audit'))
            assert len(all_rows) == len(audit_events)
            assert r'first\nsecond' in all_rows[-1][-1] and '\x1b' not in all_rows[-1][-1]
            for name, number in [('file', '1'), ('exec', '2'), ('net', '3'), ('guard', '4')]:
                named = ctl('audit', '--kind', name)
                numeric = ctl('audit', '--kind', number)
                expected = [row for row in all_rows if row[0] == name.upper()]
                assert rows(named) == rows(numeric) == expected
            selected = ctl('audit', '--kind', '2', '--domain', 'dsh-web', '--result', 'deny')
            assert rows(selected) == [all_rows[2]], selected.stdout
            assert 'lost 3 events before sequence 42' in selected.stdout  # loss must survive filtering
            assert rows(ctl('audit', '--pid', '123')) == [row for row in all_rows if row[2] == '123']
            assert rows(ctl('audit', '--result', 'allow')) == [row for row in all_rows if row[3] == 'ALLOW']
            assert rows(ctl('audit', '--domain', 'absent')) == []
            assert ctl('audit', '--result', 'failed').returncode != 0
            assert ctl('audit', '--kind', 'file', '--kind', 'exec').returncode != 0
            print('PASS all audit kinds, combined domain/result/PID filters, loss visibility and single-line arguments')

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
