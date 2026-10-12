#!/usr/bin/env python3
"""Real BPF regression for unrelated/long argv, shebangs and policy readiness.

Uses only isolated state/sockets and harmless commands; never changes production.
"""
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


def main():
    assert os.geteuid() == 0, 'run as root on a BPF LSM host'
    directory = Path(tempfile.mkdtemp(prefix='cgr-'))
    print(f'Artifacts: {directory}', flush=True)
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
rules: []
''')
    group = directory / 'group.yaml'
    secret = directory / 'ro-secret-with-a-long-path.txt'
    public = directory / 'public-with-a-long-path.txt'
    secret.write_text('secret\n')
    public.write_text('public\n')
    script = directory / 's'
    script.write_text('#!/bin/bash\nprintf "script-ok\\n"\n')
    script.chmod(0o700)
    inner = directory / 'i'
    inner.write_text('#!/bin/bash\nprintf "nested-ok\\n"\n')
    inner.chmod(0o700)
    nested = directory / 'n'
    nested.write_text(f'#!{inner}\n')
    nested.chmod(0o700)
    fallback = directory / 'f'
    token31 = 'z' * 31
    token255 = 'q' * 255
    long_executable = directory / ('e' * (255 - len(str(directory)) - 1))
    long_executable.write_bytes(Path('/usr/bin/echo').read_bytes())
    long_executable.chmod(0o700)
    long_alias = directory / 'alias'
    long_alias.symlink_to(long_executable)
    long_fallback = directory / ('f' * (255 - len(str(directory)) - 1))
    unrelated_long_path = directory / (long_fallback.name + 'x')
    unrelated_long_path.write_bytes(Path('/usr/bin/echo').read_bytes())
    unrelated_long_path.chmod(0o700)
    group.write_text((REPO / 'config/policy.dsh-default.yaml').read_text() +
                     f'  - file deny+audit {secret} [read]\n'
                     '  - exec deny+audit /usr/bin/echo blocked\n'
                     f'  - exec deny+audit /usr/bin/echo {token31}\n'
                     f'  - exec deny+audit /usr/bin/echo {token255}\n'
                     f'  - exec deny+audit {long_executable} blocked\n'
                     f'  - exec deny+audit {long_fallback} blocked\n'
                     f'  - exec deny+audit {fallback} blocked\n')
    failures = []
    events = []
    connection = None
    reader_thread = None
    log = (directory / 'daemon.log').open('w')
    daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)],
                              stdout=log, stderr=log)

    def ctl(*args):
        return subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), *args],
                              capture_output=True, text=True, timeout=15)

    def check(label, condition, detail=''):
        print(f'{"PASS" if condition else "FAIL"} {label}: {detail}', flush=True)
        if not condition:
            failures.append(label)

    def run(label, *argv, allowed=True, expected=None, stdin=None):
        result = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config),
                                 'spawn', '--domain', 'arg-test', '--', *map(str, argv)],
                                input=stdin, capture_output=True, text=True, timeout=15)
        check(label, (result.returncode == 0) == allowed and
              (not allowed or not result.stderr) and
              (expected is None or expected in result.stdout),
              f'rc={result.returncode} stderr={result.stderr.strip()}')

    try:
        for _ in range(300):
            assert daemon.poll() is None, (directory / 'daemon.log').read_text()
            status = ctl('status', '--json')
            if status.returncode == 0 and json.loads(status.stdout)['daemon_ready']:
                break
            time.sleep(0.05)
        else:
            raise AssertionError('daemon did not become ready')

        for args in [('policy', 'apply', '--policy', 'web', '--file', str(group)),
                     ('bind', '--domain', 'arg-test', '--policy', 'web')]:
            result = ctl(*args, '--json')
            assert result.returncode == 0, result.stderr
            check(' '.join(args[:2]) + ' reports actual readiness',
                  json.loads(result.stdout)['daemon_ready'])
        # It was absent when the rule was compiled: exercise string identity too.
        fallback.write_bytes(Path('/usr/bin/echo').read_bytes())
        fallback.chmod(0o700)
        long_fallback.write_bytes(Path('/usr/bin/echo').read_bytes())
        long_fallback.chmod(0o700)

        connection = socket.socket(socket.AF_UNIX)
        connection.connect(str(directory / 'events.sock'))

        def collect():
            try:
                with connection.makefile() as source:
                    for line in source:
                        events.append(json.loads(line))
            except OSError:
                pass

        reader_thread = threading.Thread(target=collect, daemon=True)
        reader_thread.start()
        run('unrelated executable with long argv', '/usr/bin/true', 'x' * 80)
        run('public long path reaches file read', '/usr/bin/cat', public, expected='public')
        run('secret is denied by FILE, not EXEC', '/usr/bin/cat', secret, allowed=False)
        run('31-byte exact argument rule', '/usr/bin/echo', token31, allowed=False)
        run('32-byte argument is not a 31-byte exact match', '/usr/bin/echo', token31 + 'x')
        run('254-byte token differs from 255-byte rule', '/usr/bin/echo', token255[:-1])
        run('255-byte exact argument rule', '/usr/bin/echo', token255, allowed=False)
        run('256-byte argument is not a 255-byte exact match', '/usr/bin/echo', token255 + 'x')
        run('255-byte executable path argument rule', long_executable, 'blocked', allowed=False)
        run('long executable inode identity survives aliases', long_alias, 'blocked', allowed=False)
        run('255-byte executable path permits unmatched argv', long_executable, 'allowed')
        run('255-byte executable fallback argument rule', long_fallback, 'blocked', allowed=False)
        run('long executable fallback permits unmatched argv', long_fallback, 'allowed')
        run('256-byte runtime path does not equal its 255-byte prefix rule',
            unrelated_long_path, 'blocked')
        run('long argument on rule subject is allowed when unmatched', '/usr/bin/echo', 'x' * 80)
        run('denied prefix survives long later argv', '/usr/bin/echo', 'blocked', 'x' * 80,
            allowed=False)
        run('denied prefix survives over-255-byte later argv', '/usr/bin/echo', 'blocked', 'x' * 300,
            allowed=False)
        run('string fallback denies prefix with long later argv', fallback, 'blocked', 'x' * 80,
            allowed=False)
        run('string fallback allows unmatched long argv', fallback, 'x' * 80)
        run('shebang survives repeated bprm checks', script, expected='script-ok')
        run('nested shebang survives repeated bprm checks', nested, expected='nested-ok')
        run('non-leader thread exec preserves shebang checks', sys.executable, '-c',
            'import os,threading; t=threading.Thread(target=lambda: os.execv(' + repr(str(script)) +
            ',[' + repr(str(script)) + '])); t.start(); t.join()', expected='script-ok')
        run('shell can start and run unrelated commands', '/bin/bash', '--noprofile', '--norc',
            stdin='sed --version >/dev/null\ncat "' + str(public) + '"\nexit\n', expected='public')
        time.sleep(0.3)
        cat_events = [event for event in events if event['kind'] == 2 and
                      event['detail'] == '/usr/bin/cat']
        check('cat executions are ALLOW', bool(cat_events) and all(e['allowed'] for e in cat_events))
        check('secret has a FILE DENY', any(e['kind'] == 1 and not e['allowed'] and
                                           secret.name in e['detail'] for e in events))
        check('audit contains the full 255-byte argument',
              any(e['kind'] == 2 and not e['allowed'] and token255 in e.get('args', []) for e in events))
        invalid = directory / 'invalid.yaml'
        invalid.write_text('rules:\n  - exec deny /usr/bin/echo ' + token255 + 'x\n')
        result = ctl('policy', 'apply', '--policy', 'invalid', '--file', str(invalid))
        check('256-byte rule is rejected explicitly', result.returncode != 0 and
              'maximum is 255' in result.stdout + result.stderr)

        # Script and interpreter policies still have real enforcement.
        original = group.read_text()
        group.write_text(original + f'  - exec deny {script} blocked\n')
        assert ctl('policy', 'apply', '--policy', 'web', '--file', str(group)).returncode == 0
        run('script argument prefix remains denied', script, 'blocked', allowed=False)
        group.write_text(original + '  - exec deny /bin/bash\n')
        assert ctl('policy', 'apply', '--policy', 'web', '--file', str(group)).returncode == 0
        run('interpreter inode deny remains enforced', script, allowed=False)
        run('unrelated binary remains usable after interpreter deny', '/usr/bin/true', 'x' * 80)
        assert not failures, failures
    finally:
        if connection is not None:
            connection.shutdown(socket.SHUT_RDWR)
            connection.close()
        if reader_thread is not None:
            reader_thread.join(timeout=3)
        (directory / 'events.json').write_text(json.dumps(events, indent=2))
        if daemon.poll() is None:
            stopped = subprocess.run([str(BIN / 'censorguardd'), 'stop', '--config', str(config)],
                                     capture_output=True, timeout=15)
            if stopped.returncode:
                daemon.terminate()
            daemon.wait(timeout=15)
        log.close()
    print('exec argument/shebang/readiness regression passed', flush=True)


if __name__ == '__main__':
    main()
