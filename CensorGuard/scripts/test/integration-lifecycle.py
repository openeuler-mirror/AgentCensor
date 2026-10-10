#!/usr/bin/env python3
"""Exercise launch/reload/stop and duplicate endpoint protection on a BPF LSM host.

Uses only isolated temporary paths and ephemeral TCP ports. Build the debug
workspace and BPF object first. Does not touch a systemd service or installed files.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
BIN = REPO / 'target/debug'


def main():
    assert os.geteuid() == 0, 'run as root on a BPF LSM host'
    directory = Path(tempfile.mkdtemp(prefix='cg-lifecycle-'))
    print(f'Artifacts: {directory}', flush=True)
    config = directory / 'base.yaml'
    secret = directory / 'secret'
    secret.write_text('secret\n')
    processes = []
    with socket.socket() as port_picker:
        port_picker.bind(('127.0.0.1', 0))
        port = port_picker.getsockname()[1]
    yaml = f'''daemon:
  bpf_object: {REPO}/bpf/enforce.bpf.o
  control_socket: {directory}/ctl.sock
  event_socket: {directory}/events.sock
  launch_socket: {directory}/launch.sock
  dsh_socket: {directory}/dsh.sock
  ui_socket: {directory}/ui.sock
  state_dir: {directory}/state
  spawn_grpc: true
  grpc_bin: {BIN}/censorguard-grpc
  grpc_listen: 127.0.0.1:{port}
rules:
  - file deny {secret}
'''
    config.write_text(yaml)

    def daemon_cmd(*args, config_path=config, ok=True):
        result = subprocess.run([str(BIN / 'censorguardd'), *args, '--config', str(config_path)],
                                capture_output=True, text=True, timeout=40)
        assert (result.returncode == 0) == ok, (args, result.stdout, result.stderr)
        return result

    def ctl(*args, ok=True):
        result = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), *args],
                                capture_output=True, text=True, timeout=10)
        assert (result.returncode == 0) == ok, (args, result.stdout, result.stderr)
        return json.loads(result.stdout) if ok else result

    def rpc_status():
        result = subprocess.run(['node', str(REPO / 'scripts/censorguard-grpc-call.mjs'),
                                 f'127.0.0.1:{port}', 'Status', '{}'],
                                capture_output=True, text=True, timeout=10)
        assert result.returncode == 0, (result.stdout, result.stderr)
        return json.loads(result.stdout)

    def launch(label):
        log = directory / f'{label}.log'
        with log.open('w') as output:
            process = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)],
                                       stdout=output, stderr=output)
        processes.append(process)
        for _ in range(300):
            assert process.poll() is None, log.read_text()[-6000:]
            if '[READY] 32 required hooks attached' in log.read_text():
                status = ctl('status')
                assert status['daemon_ready'], status
                return process, log
            time.sleep(0.05)
        raise AssertionError(log.read_text())

    try:
        for binary in ['censorguardd', 'censorguardctl', 'censorguard-grpc']:
            result = subprocess.run([str(BIN / binary), '--version'], capture_output=True, text=True, check=True)
            assert result.stdout.strip() == f'{binary} 0.5.0', result.stdout
        missing = daemon_cmd('reload', ok=False)
        assert 'no running daemon' in missing.stderr, missing.stderr
        assert 'no running daemon' in daemon_cmd('stop', ok=False).stderr
        print('PASS versions and missing-daemon diagnostics', flush=True)

        # Legacy instances might not have a lock file: refuse to steal a live socket anyway.
        with socket.socket(socket.AF_UNIX) as legacy:
            legacy.bind(str(directory / 'ctl.sock'))
            legacy.listen()
            inode = (directory / 'ctl.sock').stat().st_ino
            conflict = daemon_cmd('launch', ok=False)
            assert 'existing service is listening' in conflict.stderr, conflict.stderr
            assert (directory / 'ctl.sock').stat().st_ino == inode
        (directory / 'ctl.sock').unlink()
        print('PASS active legacy socket is preserved', flush=True)

        with socket.socket() as occupied:
            occupied.bind(('127.0.0.1', port))
            occupied.listen()
            conflict = daemon_cmd('launch', ok=False)
            assert 'gRPC address' in conflict.stderr and 'unavailable' in conflict.stderr, conflict.stderr
            assert '[READY]' not in conflict.stdout + conflict.stderr
            adapter = subprocess.run([str(BIN / 'censorguard-grpc'), '--config', str(config)],
                                     capture_output=True, text=True, timeout=10)
            assert adapter.returncode != 0 and '[READY]' not in adapter.stdout, adapter
        print('PASS occupied port rejects daemon/adapter startup without false READY', flush=True)

        # Initial adapter failures must clean up sockets and stop, not loop forever.
        config.write_text(yaml.replace(str(BIN / 'censorguard-grpc'), '/bin/false'))
        failed = daemon_cmd('launch', ok=False)
        assert 'gRPC did not become ready' in failed.stderr, failed.stderr
        assert '[READY]' not in failed.stdout + failed.stderr
        assert not any((directory / f'{name}.sock').exists() for name in ['ctl', 'events', 'launch', 'dsh', 'ui'])
        config.write_text(yaml)
        print('PASS adapter startup failure exits and cleans up owned endpoints', flush=True)

        daemon, log = launch('first')
        initial = ctl('status')
        assert rpc_status()['daemonBootId'] == initial['daemon_boot_id']
        inode = (directory / 'ctl.sock').stat().st_ino
        duplicate = daemon_cmd('launch', ok=False)
        assert 'another daemon owns' in duplicate.stderr, duplicate.stderr
        assert (directory / 'ctl.sock').stat().st_ino == inode
        assert ctl('status')['daemon_boot_id'] == initial['daemon_boot_id']
        # Distinct socket paths also cannot concurrently own the same revision store.
        alias = directory / 'alias.yaml'
        alias.write_text(yaml.replace('/ctl.sock', '/other-ctl.sock').replace('/events.sock', '/other-events.sock')
                         .replace('/launch.sock', '/other-launch.sock').replace('/dsh.sock', '/other-dsh.sock')
                         .replace('/ui.sock', '/other-ui.sock'))
        assert 'another daemon owns' in daemon_cmd('launch', config_path=alias, ok=False).stderr
        copy = directory / 'copy.yaml'
        copy.write_text(yaml)
        assert 'running daemon uses' in daemon_cmd('reload', config_path=copy, ok=False).stderr
        print('PASS duplicate launch/state ownership and wrong-config reload rejection', flush=True)

        denied = ctl('spawn', '--domain', 'probe', '--', '/usr/bin/cat', str(secret), ok=False)
        assert denied.returncode != 0
        group = directory / 'worker.yaml'
        group.write_text('rules:\n  - exec deny /usr/bin/id\n')
        ctl('policy', 'apply', '--policy', 'worker', '--file', str(group))
        ctl('bind', '--domain', 'probe', '--policy', 'worker')
        before = ctl('status')
        config.write_text(yaml.replace('  spawn_grpc: true', '  audit_exec: true\n  spawn_grpc: true')
                          .replace(f'  - file deny {secret}', '  - exec deny /usr/bin/whoami'))
        reloaded = daemon_cmd('reload')
        after = ctl('status')
        assert 'daemon restarted' in reloaded.stdout, reloaded.stdout
        assert after['daemon_boot_id'] != before['daemon_boot_id'] and after['daemon_ready']
        assert daemon.poll() is None and after['audit_exec'] and after['version'] == before['version'] + 1
        assert rpc_status()['daemonBootId'] == after['daemon_boot_id']
        allowed = subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), 'spawn',
                                  '--domain', 'probe', '--', '/usr/bin/cat', str(secret)], capture_output=True)
        assert allowed.returncode == 0, allowed
        ctl('spawn', '--domain', 'probe', '--', '/usr/bin/whoami', ok=False)
        ctl('spawn', '--domain', 'probe', '--', '/usr/bin/id', ok=False)
        print('PASS reload waits for new READY, reloads base/settings and restores group bindings', flush=True)

        stopped = daemon_cmd('stop')
        assert 'daemon stopped' in stopped.stdout, stopped.stdout
        assert daemon.wait(timeout=5) == 0
        assert not (directory / 'ctl.sock').exists()
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', port))
        assert 'no running daemon' in daemon_cmd('reload', ok=False).stderr
        print('PASS stop exits daemon, frees gRPC port, and reload after stop reports missing instance', flush=True)

        daemon, log = launch('second')
        assert rpc_status()['daemonBootId'] == ctl('status')['daemon_boot_id']
        daemon_cmd('stop')
        assert daemon.wait(timeout=5) == 0
        print('PASS launch works again after stop; lifecycle acceptance passed', flush=True)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == '__main__':
    main()
