#!/usr/bin/env python3
"""Generate deterministic process, file, IPC, network, mmap, and stdio load."""

from __future__ import annotations

import argparse
import json
import mmap
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path


def child_main(size: int) -> int:
    payload = b"censorscope-benchmark-child\n" * max(1, size // 28)
    sys.stdout.buffer.write(payload[:size])
    sys.stdout.buffer.flush()
    return 0


def tcp_roundtrip(payload: bytes) -> None:
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    accepted: list[socket.socket] = []

    def accept_once() -> None:
        connection, _ = listener.accept()
        accepted.append(connection)
        remaining = len(payload)
        while remaining:
            block = connection.recv(min(65536, remaining))
            if not block:
                break
            remaining -= len(block)
        connection.sendall(b"ok")
        connection.close()

    thread = threading.Thread(target=accept_once, daemon=True)
    thread.start()
    client = socket.create_connection(listener.getsockname(), timeout=5)
    client.sendall(payload)
    client.recv(2)
    client.close()
    thread.join(timeout=5)
    listener.close()


def one_iteration(root: Path, file_ops: int, payload_bytes: int, children: int, seed: int) -> dict[str, int]:
    root.mkdir(parents=True, exist_ok=True)
    payload = bytes(((index + seed) % 251 for index in range(payload_bytes)))
    file_count = 0
    for index in range(file_ops):
        path = root / f"event-{index % 64}.bin"
        with path.open("wb") as handle:
            handle.write(payload)
            handle.flush()
        with path.open("rb") as handle:
            handle.read()
        file_count += 1
    mmap_path = root / "mapped.bin"
    mmap_path.write_bytes(payload[: max(1, min(payload_bytes, 1 << 20))])
    with mmap_path.open("r+b") as handle:
        mapped = mmap.mmap(handle.fileno(), 0)
        _ = mapped[: min(4096, len(mapped))]
        mapped.close()
    read_fd, write_fd = os.pipe()
    os.write(write_fd, payload[: min(len(payload), 65536)])
    os.read(read_fd, min(len(payload), 65536))
    os.close(read_fd)
    os.close(write_fd)
    skipped = 0
    try:
        left, right = socket.socketpair()
        left.sendall(payload[: min(len(payload), 65536)])
        right.recv(min(len(payload), 65536))
        left.close()
        right.close()
    except OSError:
        skipped += 1
    try:
        tcp_roundtrip(payload[: min(len(payload), 1 << 20)])
    except OSError:
        skipped += 1
    child_size = min(payload_bytes, 1 << 20)
    for _ in range(children):
        subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), "--child", str(child_size)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
    return {
        "file_operations": file_count,
        "payload_bytes": payload_bytes,
        "children": children,
        "skipped_operations": skipped,
    }


def run(args: argparse.Namespace) -> int:
    root = Path(args.output).resolve()
    root.mkdir(parents=True, exist_ok=True)
    start = time.monotonic()
    iterations = args.iterations
    completed = 0
    totals = {"file_operations": 0, "payload_bytes": 0, "children": 0, "skipped_operations": 0}
    with ThreadPoolExecutor(max_workers=args.concurrency) as executor:
        while completed < iterations or (args.duration_seconds and time.monotonic() - start < args.duration_seconds):
            batch = min(args.concurrency, max(1, iterations - completed)) if not args.duration_seconds else args.concurrency
            futures = [
                executor.submit(
                    one_iteration,
                    root / f"worker-{index}",
                    args.file_operations,
                    args.payload_bytes,
                    args.children,
                    args.seed,
                )
                for index in range(batch)
            ]
            for future in futures:
                result = future.result()
                completed += 1
                for key, value in result.items():
                    totals[key] += value
    marker = {
        "ok": True,
        "profile": args.profile,
        "iterations": completed,
        "duration_seconds": time.monotonic() - start,
        "totals": totals,
    }
    (root / "complete.json").write_text(json.dumps(marker, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(marker, sort_keys=True), flush=True)
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--child", type=int)
    parser.add_argument("--profile", default="custom")
    parser.add_argument("--output", required=False, default=tempfile.gettempdir())
    parser.add_argument("--iterations", type=int, default=1)
    parser.add_argument("--duration-seconds", type=float, default=0.0)
    parser.add_argument("--file-operations", type=int, default=10)
    parser.add_argument("--payload-bytes", type=int, default=4096)
    parser.add_argument("--children", type=int, default=1)
    parser.add_argument("--concurrency", type=int, default=1)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()
    if args.child is not None:
        raise SystemExit(child_main(args.child))
    for name in ("iterations", "file_operations", "payload_bytes", "children", "concurrency"):
        if getattr(args, name) < 1:
            parser.error(f"--{name.replace('_', '-')} must be positive")
    if args.duration_seconds < 0:
        parser.error("--duration-seconds cannot be negative")
    return args


if __name__ == "__main__":
    raise SystemExit(run(parse_args()))
