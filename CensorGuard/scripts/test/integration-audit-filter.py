#!/usr/bin/env python3
"""Compare CLI filters with real BPF decisions, using isolated config and processes."""
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
BIN = REPO / 'target/release'


def wait_for(condition, message):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(0.05)
    raise AssertionError(message)


def main():
    assert os.geteuid() == 0, 'run as root on a BPF LSM host'
    directory = Path(tempfile.mkdtemp(prefix='cg-audit-filter-'))
    print(f'Artifacts: {directory}', flush=True)
    secret = directory / 'ro-secret.txt'
    locked = directory / 'ww-nodelete.txt'
    secret.write_text('secret\n')
    locked.write_text('keep\n')
    config = directory / 'base.yaml'
    config.write_text(f'''daemon:
  bpf_object: {REPO}/bpf/enforce.bpf.o
  control_socket: {directory}/ctl.sock
  event_socket: {directory}/events.sock
  launch_socket: {directory}/launch.sock
  dsh_socket: {directory}/dsh.sock
  ui_socket: {directory}/ui.sock
  state_dir: {directory}/state
  audit_file: true
  audit_exec: true
rules:
  - file deny {secret} [read]
  - file deny {locked} [delete]
  - exec deny /usr/bin/echo blocked
''')
    probe = directory / 'probe.py'
    probe.write_text('''import os, sys
print(f'ready={os.getpid()}', flush=True)
sys.stdin.readline()
os.execv('/usr/bin/cat', ['/usr/bin/cat', sys.argv[1]])
''')
    consumers = []
    output_files = []
    daemon = agent = raw = reader_thread = None
    raw_events = []
    daemon_log = (directory / 'daemon.log').open('w')

    def ctl(*args):
        return subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), *args],
                              text=True, capture_output=True, timeout=12)

    def status():
        result = ctl('status', '--json')
        return json.loads(result.stdout) if result.returncode == 0 else {}

    def read_raw():
        try:
            with raw.makefile('r') as stream:
                for line in stream:
                    raw_events.append(json.loads(line))
        except OSError:
            pass

    def rows(name):
        return [line.split(maxsplit=6) for line in (directory / f'{name}.log').read_text().splitlines()
                if line.startswith(('FILE ', 'EXEC ', 'NET ', 'GUARD '))]

    def run(*command):
        result = ctl('spawn', '--domain', 'dsh-web', '--', *map(str, command))
        assert result.returncode != 0 and 'Operation not permitted' in result.stderr, result

    try:
        daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)],
                                  stdout=daemon_log, stderr=daemon_log)
        wait_for(lambda: (directory / 'ctl.sock').exists() and status().get('daemon_ready'), 'daemon not ready')
        agent = subprocess.Popen([str(BIN / 'censorguardctl'), '--config', str(config),
                                  'spawn', '--domain', 'dsh-web', '--', sys.executable, str(probe), str(secret)],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        while True:
            line = agent.stdout.readline()
            assert line, 'probe exited before becoming ready'
            if line.startswith('ready='):
                cat_pid = int(line.split('=')[1])
                break

        raw = socket.socket(socket.AF_UNIX)
        raw.connect(str(directory / 'events.sock'))
        reader_thread = threading.Thread(target=read_raw, daemon=True)
        reader_thread.start()
        for name, args in [
            ('all', []), ('exec-number', ['--kind', '2']), ('exec-name', ['--kind', 'exec']),
            ('file', ['--kind', 'file']),
            ('denied', ['--domain', 'dsh-web', '--result', 'deny']),
            ('exec-denied', ['--domain', 'dsh-web', '--kind', 'exec', '--result', 'deny']),
            ('pid', ['--pid', str(cat_pid)]),
        ]:
            output = (directory / f'{name}.log').open('w')
            output_files.append(output)
            process = subprocess.Popen([str(BIN / 'censorguardctl'), '--config', str(config), 'audit', *args],
                                       stdout=output, stderr=subprocess.PIPE, text=True)
            consumers.append(process)
        wait_for(lambda: status().get('event_subscribers') == 8, 'audit subscribers not registered')
        for process in consumers:
            assert process.poll() is None

        agent.stdin.write('\n')
        agent.stdin.flush()
        stdout, stderr = agent.communicate(timeout=10)
        assert agent.returncode != 0 and 'Operation not permitted' in stderr, (stdout, stderr)
        run('/usr/bin/cat', secret)
        run('/usr/bin/rm', '-rf', locked)
        run('/usr/bin/echo', 'blocked')
        assert locked.read_text() == 'keep\n', 'denied rm must preserve the protected file'
        other = ctl('spawn', '--domain', 'other-domain', '--', '/usr/bin/cat', str(secret))
        assert other.returncode != 0 and 'Operation not permitted' in other.stderr, other

        wait_for(lambda: any(event['kind'] == 2 and not event['allowed'] and event['detail'] == '/usr/bin/echo'
                             for event in raw_events), 'real EXEC DENY missing')
        wait_for(lambda: any(event['kind'] == 1 and not event['allowed'] and 'ww-nodelete.txt' in event['detail']
                             for event in raw_events), 'real FILE unlink DENY missing')
        wait_for(lambda: len(rows('all')) == len(raw_events), 'CLI did not drain the real event stream')
        # Shut down the isolated publisher to let every audit subscriber consume
        # all queued events and exit naturally; then compare exact stream contents.
        stopped = subprocess.run([str(BIN / 'censorguardd'), 'stop', '--config', str(config)],
                                 text=True, capture_output=True, timeout=40)
        assert stopped.returncode == 0, stopped
        assert daemon.wait(timeout=5) == 0
        for process in consumers:
            assert process.wait(timeout=5) == 0, process.stderr.read()
        reader_thread.join(timeout=3)

        all_rows = rows('all')
        assert len(all_rows) == len(raw_events)
        assert rows('exec-name') == rows('exec-number') == [row for row in all_rows if row[0] == 'EXEC']
        assert rows('file') == [row for row in all_rows if row[0] == 'FILE']
        assert rows('denied') == [row for row in all_rows if row[3] == 'DENY' and row[4] == 'dsh-web']
        assert rows('exec-denied') == [row for row in rows('denied') if row[0] == 'EXEC']
        assert rows('pid') == [row for row in all_rows if row[2] == str(cat_pid)]
        cat_rows = rows('pid')
        assert any(row[0] == 'EXEC' and row[3] == 'ALLOW' and '/usr/bin/cat' in row[6] for row in cat_rows), cat_rows
        assert any(row[0] == 'FILE' and row[3] == 'DENY' and 'ro-secret.txt' in row[6] for row in cat_rows), cat_rows
        assert not any(row[0] == 'EXEC' and row[3] == 'DENY' for row in cat_rows), cat_rows
        rm_rows = [row for row in all_rows if 'ww-nodelete.txt' in row[6]]
        assert any(row[0] == 'EXEC' and row[3] == 'ALLOW' for row in rm_rows), rm_rows
        assert any(row[0] == 'FILE' and row[1] == 'unlink' and row[3] == 'DENY' for row in rm_rows), rm_rows
        assert rows('exec-denied') and all('/usr/bin/echo' in row[6] for row in rows('exec-denied'))
        (directory / 'events.json').write_text(json.dumps(raw_events, indent=2))
        print('PASS denied cat/rm: real EXEC ALLOW and FILE DENY share PID; the operation fails and file remains', flush=True)
        print('PASS exec deny rule: actual execution failure produces EXEC DENY', flush=True)
        print('PASS real CLI streams: numeric/named kind, domain, result and PID filters exactly match raw BPF events', flush=True)
    finally:
        for process in [agent, *consumers, daemon]:
            if process is not None and process.poll() is None:
                process.terminate()
                process.wait(timeout=8)
        if raw is not None:
            raw.close()
        for output in output_files:
            output.close()
        daemon_log.close()


if __name__ == '__main__':
    main()
