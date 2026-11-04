#!/usr/bin/env python3
"""Actual IPv4, IPv6 and mapped IPv6 network operations, including enable_net."""
import errno
import ctypes
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[2]
BIN = REPO / 'target/debug'
ENDPOINTS = (
    (socket.AF_INET, '127.0.0.1'),
    (socket.AF_INET6, '::1'),
    (socket.AF_INET6, '::ffff:127.0.0.1'),
)


def probe(action, family, address, port, ready_path):
    family = int(family)
    port = int(port)
    kind = socket.SOCK_DGRAM if action in ('udp', 'udp4') else socket.SOCK_STREAM
    with socket.socket(family, kind) as stream:
        stream.settimeout(2)
        result = 0
        try:
            if action == 'connect':
                stream.connect((address, port))
            elif action == 'bind':
                stream.bind((address, port))
            elif action == 'udp':
                stream.sendto(b'network-probe', (address, port))
            elif action == 'udp4':
                sendto = ctypes.CDLL(None, use_errno=True).sendto
                sendto.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_size_t,
                                   ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
                sendto.restype = ctypes.c_ssize_t
                target = struct.pack('=HH4s8x', socket.AF_INET, socket.htons(port),
                                     socket.inet_aton('127.0.0.1'))
                if sendto(stream.fileno(), b'network-probe', 13, 0, target, len(target)) < 0:
                    result = ctypes.get_errno()
            elif action == 'accept':
                stream.bind((address, 0))
                stream.listen()
                Path(ready_path).write_text(str(stream.getsockname()[1]))
                connection, _ = stream.accept()
                connection.close()
        except OSError as error:
            result = error.errno or errno.ETIMEDOUT
        print(json.dumps({'errno': result}), flush=True)


def held_listener(family, address):
    family = int(family)
    connection = None
    with socket.socket(family) as listener:
        listener.bind((address, 0))
        listener.listen()
        listener.settimeout(1)
        print(json.dumps({'port': listener.getsockname()[1]}), flush=True)
        for action in sys.stdin:
            result = 0
            try:
                if action.strip() == 'accept':
                    connection, _ = listener.accept()
                    connection.settimeout(1)
                elif action.strip() == 'recv':
                    connection.recv(10)
                elif action.strip() == 'send':
                    connection.send(b'reply')
                elif action.strip() == 'exit':
                    break
            except OSError as error:
                result = error.errno or errno.ETIMEDOUT
            print(json.dumps({'errno': result}), flush=True)
    if connection:
        connection.close()


def main():
    assert os.geteuid() == 0, 'requires root and BPF LSM'
    directory = Path(tempfile.mkdtemp(prefix='cg-network-ops-'))
    print(f'Artifacts: {directory}', flush=True)
    config = directory / 'base.yaml'
    resources = []
    processes = []
    daemon = None
    event_stream = None
    events = []
    cases = []
    rules = []
    inherited_peers = []
    for family, address in ENDPOINTS:
        mapped = address.startswith('::ffff:')
        peer_family = socket.AF_INET if mapped else family
        peer_address = '127.0.0.1' if mapped else address
        peers = {}
        actions = ('connect', 'bind', 'udp', 'accept', 'udp4') if mapped else ('connect', 'bind', 'udp', 'accept')
        for action in actions:
            stream = socket.socket(peer_family, socket.SOCK_DGRAM if action in ('udp', 'udp4') else socket.SOCK_STREAM)
            stream.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            stream.bind((peer_address, 0))
            port = stream.getsockname()[1]
            target = f'{peer_address}:{port}' if peer_family == socket.AF_INET else f'[{peer_address}]:{port}'
            rules.append(f'  - net deny {target}')
            if action == 'connect':
                stream.listen(128)
            if action == 'bind':
                stream.close()  # A denied bind must be on a genuinely available address.
            else:
                resources.append(stream)
                peers[action] = stream
            cases.append((family, action, address, port, peers))
        for _ in range(2):
            peer = socket.socket(peer_family)
            peer.bind((peer_address, 0))
            peer.settimeout(0.8)
            port = peer.getsockname()[1]
            target = f'{peer_address}:{port}' if peer_family == socket.AF_INET else f'[{peer_address}]:{port}'
            rules.append(f'  - net deny {target}')
            resources.append(peer)
            inherited_peers.append((family, address, peer))

    def write_config(enabled):
        config.write_text(f'''daemon:
  bpf_object: {REPO}/bpf/enforce.bpf.o
  control_socket: {directory}/ctl.sock
  event_socket: {directory}/events.sock
  launch_socket: {directory}/launch.sock
  dsh_socket: {directory}/dsh.sock
  ui_socket: {directory}/ui.sock
  state_dir: {directory}/state
  enable_net: {str(enabled).lower()}
  audit_net: true
rules:
''' + '\n'.join(rules) + '\n')

    def ctl(*args):
        return subprocess.run([str(BIN / 'censorguardctl'), '--config', str(config), *args],
                              text=True, capture_output=True, timeout=15, check=True)

    def collect():
        nonlocal event_stream
        event_stream = socket.socket(socket.AF_UNIX)
        event_stream.connect(str(directory / 'events.sock'))
        stream = event_stream
        def read():
            with stream.makefile('r') as source:
                for line in source:
                    events.append(json.loads(line))
        threading.Thread(target=read, daemon=True).start()
        time.sleep(0.1)

    def exercise(enabled):
        for family, action, address, port, peers in cases:
            ready_path = directory / f'accept-ready-{address}'
            ready_path.unlink(missing_ok=True)
            process = subprocess.Popen([str(BIN / 'censorguardctl'), '--config', str(config),
                                        'spawn', '--domain', 'network-ops', '--', sys.executable,
                                        str(Path(__file__).resolve()), '--probe', action, str(family), address, str(port), str(ready_path)],
                                       text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            processes.append(process)
            if action == 'accept':
                deadline = time.monotonic() + 10
                while not ready_path.exists() and process.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.01)
                assert ready_path.exists(), 'accept readiness timeout'
                peer = peers['accept']
                peer.settimeout(0.8)
                try:
                    peer.connect((peer.getsockname()[0], int(ready_path.read_text())))
                    assert not enabled, 'denied inbound handshake unexpectedly completed'
                except OSError:
                    assert enabled, 'allowed inbound handshake failed'
            stdout, stderr = process.communicate(timeout=12)
            (directory / f'{address}-{action}-{enabled}.log').write_text(stdout + stderr)
            assert process.returncode == 0, (stdout, stderr)
            result = json.loads(next(line for line in stdout.splitlines() if line.startswith('{')))
            expected = 0 if not enabled else (errno.ETIMEDOUT if action == 'accept' else errno.EPERM)
            assert result['errno'] == expected, (family, action, enabled, result, stderr)
            if not enabled and action in ('udp', 'udp4'):
                peers[action].settimeout(3)
                assert peers[action].recv(100) == b'network-probe'
            print(f'PASS address={address} {action}: enable_net={enabled}, errno={expected}', flush=True)

    def existing_sockets():
        for family, address in ENDPOINTS:
            process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                        '--held-listener', str(family), address], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            processes.append(process)
            port = json.loads(process.stdout.readline())['port']
            peers = [(candidate_address, peer) for candidate, candidate_address, peer in inherited_peers
                     if candidate == family and candidate_address == address]
            address, connected = peers[0]
            connected.connect((connected.getsockname()[0], port))  # Established before process registration.
            def command(action, expected):
                process.stdin.write(action + '\n')
                process.stdin.flush()
                result = json.loads(process.stdout.readline())
                assert result['errno'] == expected, (family, action, result)
            command('accept', 0)
            ctl('attach', '--pid', str(process.pid), '--domain', f'existing-{family}')
            connected.send(b'queued-data')
            command('recv', errno.EPERM)
            command('send', errno.EPERM)
            address, incoming = peers[1]
            try:
                incoming.connect((incoming.getsockname()[0], port))
                raise AssertionError('existing listener accepted a denied new handshake')
            except OSError:
                pass
            command('accept', errno.ETIMEDOUT)
            print(f'PASS address={address}: attached existing listener rejects inbound; pre-existing connection send/recv denied', flush=True)
            # Reload destroys old BPF ownership. Reattachment must seed the same listening fd.
            if family == socket.AF_INET:
                event_stream.shutdown(socket.SHUT_RDWR)
                event_stream.close()
                subprocess.run([str(BIN / 'censorguardd'), 'reload', '--config', str(config)],
                               check=True, timeout=40)
                ctl('attach', '--pid', str(process.pid), '--domain', f'existing-{family}')
                collect()
                command('recv', errno.EPERM)
                command('send', errno.EPERM)
                print('PASS reload/reattach seeds existing socket ownership and enforces connected I/O', flush=True)
            process.stdin.write('exit\n')
            process.stdin.flush()
            assert process.wait(timeout=5) == 0

    try:
        write_config(True)
        with (directory / 'daemon.log').open('w') as output:
            daemon = subprocess.Popen([str(BIN / 'censorguardd'), 'launch', '--config', str(config)],
                                      stdout=output, stderr=output)
        for _ in range(300):
            assert daemon.poll() is None, (directory / 'daemon.log').read_text()
            if (directory / 'ctl.sock').exists():
                if json.loads(ctl('status', '--json').stdout)['daemon_ready']:
                    break
            time.sleep(0.05)
        else:
            raise AssertionError('daemon not ready')
        collect()
        existing_sockets()
        exercise(True)
        time.sleep(0.3)
        denied = [event for event in events if event['kind'] == 3 and not event['allowed']]
        assert {event['op'] for event in denied} == {0, 1, 2, 3, 4}, denied
        assert len(denied) >= 8, denied
        assert all(event['domain'] in ('network-ops', 'existing-2', 'existing-10') for event in denied), denied
        assert all(event['tgid'] != os.getpid() for event in denied), denied
        print('PASS actual NET DENY events for connect/bind/sendmsg/accept on IPv4, IPv6 and mapped IPv6', flush=True)
        event_stream.shutdown(socket.SHUT_RDWR)
        event_stream.close()
        write_config(False)
        subprocess.run([str(BIN / 'censorguardd'), 'reload', '--config', str(config)], check=True, timeout=40)
        # Recreate accept peers: the earlier TCP connections have been consumed/refused.
        for family, action, address, port, peers in cases:
            if action == 'accept':
                peer_family = peers['accept'].family
                peers['accept'].close()
                peer = socket.socket(peer_family, socket.SOCK_STREAM)
                peer.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                peer.bind(('127.0.0.1' if peer_family == socket.AF_INET else address, port))
                peers['accept'] = peer
                resources.append(peer)
        checkpoint = len(events)
        collect()
        exercise(False)
        time.sleep(0.3)
        assert not any(event['kind'] == 3 for event in events[checkpoint:]), events[checkpoint:]
        print('PASS enable_net=false permits every operation and suppresses network hook events', flush=True)
        subprocess.run([str(BIN / 'censorguardd'), 'stop', '--config', str(config)], check=True, timeout=40)
        assert daemon.wait(timeout=5) == 0
    finally:
        if event_stream:
            event_stream.shutdown(socket.SHUT_RDWR)
            event_stream.close()
        for process in [*processes, daemon]:
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        for stream in resources:
            stream.close()
        (directory / 'events.json').write_text(json.dumps(events, indent=2))


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--probe':
        probe(*sys.argv[2:])
    elif len(sys.argv) > 1 and sys.argv[1] == '--held-listener':
        held_listener(*sys.argv[2:])
    else:
        main()
