from __future__ import annotations

import os
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import IO, Iterable


@dataclass(frozen=True)
class ProcessMemory:
    pid: int
    ppid: int
    rss_bytes: int
    pss_bytes: int | None
    command: str


def read_ppid(pid: int) -> int | None:
    try:
        raw = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
        _, fields = raw.rsplit(")", 1)
        return int(fields.split()[1])
    except (OSError, ValueError, IndexError):
        return None


def process_tree(root_pid: int) -> set[int]:
    parents: dict[int, int] = {}
    try:
        entries = list(Path("/proc").iterdir())
    except OSError:
        return set()
    for entry in entries:
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        ppid = read_ppid(pid)
        if ppid is not None:
            parents[pid] = ppid
    descendants = {root_pid} if root_pid in parents or Path(f"/proc/{root_pid}").exists() else set()
    changed = True
    while changed:
        changed = False
        for pid, ppid in parents.items():
            if ppid in descendants and pid not in descendants:
                descendants.add(pid)
                changed = True
    return descendants


def read_memory(pid: int) -> ProcessMemory | None:
    rss_bytes = None
    pss_bytes = None
    try:
        for line in Path(f"/proc/{pid}/status").read_text(encoding="utf-8").splitlines():
            if line.startswith("VmRSS:"):
                rss_bytes = int(line.split()[1]) * 1024
                break
    except (OSError, ValueError, IndexError):
        return None
    if rss_bytes is None:
        return None
    try:
        for line in Path(f"/proc/{pid}/smaps_rollup").read_text(encoding="utf-8").splitlines():
            if line.startswith("Pss:"):
                pss_bytes = int(line.split()[1]) * 1024
                break
    except (OSError, ValueError, IndexError, PermissionError):
        pass
    try:
        command = Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ").decode(
            "utf-8", errors="replace"
        ).strip()
    except OSError:
        command = ""
    return ProcessMemory(
        pid=pid,
        ppid=read_ppid(pid) or 0,
        rss_bytes=rss_bytes,
        pss_bytes=pss_bytes,
        command=command,
    )


def tree_memory(root_pid: int) -> tuple[list[ProcessMemory], int, int | None]:
    rows = [row for pid in process_tree(root_pid) if (row := read_memory(pid)) is not None]
    rss = sum(row.rss_bytes for row in rows)
    pss_values = [row.pss_bytes for row in rows]
    pss = sum(value for value in pss_values if value is not None) if any(
        value is not None for value in pss_values
    ) else None
    return rows, rss, pss


def wait_for_state(pid: int, state: str, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            lines = Path(f"/proc/{pid}/status").read_text(encoding="utf-8").splitlines()
        except OSError:
            break
        current = next((line.split()[1] for line in lines if line.startswith("State:")), "")
        if current == state:
            return
        time.sleep(0.02)
    raise TimeoutError(f"process {pid} did not enter state {state}")


def spawn_paused_exec(
    argv: list[str],
    env: dict[str, str],
    stdout: IO[str],
    stderr: IO[str],
) -> subprocess.Popen[str]:
    shell = ["/bin/sh", "-c", 'kill -STOP $$; exec "$@"', "benchmark-agent", *argv]
    process = subprocess.Popen(
        shell,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=stdout,
        stderr=stderr,
        text=True,
        start_new_session=True,
    )
    wait_for_state(process.pid, "T", 10.0)
    return process


def continue_process(process: subprocess.Popen[str]) -> None:
    os.kill(process.pid, signal.SIGCONT)


def terminate_process_group(process: subprocess.Popen[str], grace_seconds: float = 10.0) -> int:
    if process.poll() is not None:
        return int(process.returncode or 0)
    try:
        os.killpg(process.pid, signal.SIGINT)
    except ProcessLookupError:
        return int(process.poll() or 0)
    try:
        return process.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            return int(process.poll() or 0)
    try:
        return process.wait(timeout=5.0)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        return process.wait(timeout=5.0)


def command_output(argv: Iterable[str], timeout: float = 30.0) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(argv),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
