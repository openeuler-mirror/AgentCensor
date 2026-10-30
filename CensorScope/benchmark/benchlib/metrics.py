from __future__ import annotations

import csv
import json
import math
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .process import tree_memory


@dataclass
class RootProcess:
    name: str
    pid: int


@dataclass
class MemorySampler:
    output: Path
    interval: float
    roots: list[RootProcess] = field(default_factory=list)
    size_paths: dict[str, Path] = field(default_factory=dict)
    _stop: threading.Event = field(default_factory=threading.Event, init=False)
    _thread: threading.Thread | None = field(default=None, init=False)
    _started: float = field(default=0.0, init=False)
    samples: list[dict[str, Any]] = field(default_factory=list, init=False)

    def start(self) -> None:
        self._started = time.monotonic()
        self._thread = threading.Thread(target=self._run, name="benchmark-memory-sampler", daemon=True)
        self._thread.start()

    def add_root(self, name: str, pid: int) -> None:
        self.roots.append(RootProcess(name, pid))

    def stop(self) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=max(5.0, self.interval * 2))
        self.write_csv()

    def _run(self) -> None:
        while not self._stop.is_set():
            row: dict[str, Any] = {"elapsed_seconds": time.monotonic() - self._started}
            total_rss = 0
            total_pss = 0
            has_pss = False
            total_processes = 0
            for root in list(self.roots):
                processes, rss, pss = tree_memory(root.pid)
                row[f"{root.name}_pid"] = root.pid
                row[f"{root.name}_processes"] = len(processes)
                row[f"{root.name}_rss_bytes"] = rss
                row[f"{root.name}_pss_bytes"] = pss
                total_rss += rss
                total_processes += len(processes)
                if pss is not None:
                    total_pss += pss
                    has_pss = True
            row["total_processes"] = total_processes
            row["total_rss_bytes"] = total_rss
            row["total_pss_bytes"] = total_pss if has_pss else None
            for name, path in self.size_paths.items():
                if path.is_dir():
                    size = sum(item.stat().st_size for item in path.glob("**/*") if item.is_file())
                else:
                    try:
                        size = path.stat().st_size
                    except OSError:
                        size = 0
                row[f"{name}_bytes"] = size
            self.samples.append(row)
            self._stop.wait(self.interval)

    def write_csv(self) -> None:
        self.output.parent.mkdir(parents=True, exist_ok=True)
        keys: list[str] = []
        for row in self.samples:
            for key in row:
                if key not in keys:
                    keys.append(key)
        with self.output.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.DictWriter(handle, fieldnames=keys)
            writer.writeheader()
            writer.writerows(self.samples)


def linear_slope(points: list[tuple[float, float]]) -> float:
    if len(points) < 2:
        return 0.0
    mean_x = sum(x for x, _ in points) / len(points)
    mean_y = sum(y for _, y in points) / len(points)
    denominator = sum((x - mean_x) ** 2 for x, _ in points)
    if denominator == 0:
        return 0.0
    return sum((x - mean_x) * (y - mean_y) for x, y in points) / denominator


def memory_summary(
    samples: list[dict[str, Any]],
    tail_fraction: float,
    max_tail_growth_ratio: float,
) -> dict[str, Any]:
    usable = [row for row in samples if row.get("total_rss_bytes") is not None]
    if not usable:
        return {"sample_count": 0, "stable": False, "reason": "no memory samples"}
    tail_count = max(3, math.ceil(len(usable) * tail_fraction))
    tail = usable[-tail_count:]
    rss_values = [int(row["total_rss_bytes"]) for row in usable]
    tail_values = [int(row["total_rss_bytes"]) for row in tail]
    points = [(float(row["elapsed_seconds"]), float(row["total_rss_bytes"])) for row in tail]
    peak = max(rss_values)
    tail_growth = max(0, max(tail_values) - tail_values[0])
    growth_ratio = tail_growth / peak if peak else 0.0
    return {
        "sample_count": len(usable),
        "baseline_rss_bytes": rss_values[0],
        "peak_rss_bytes": peak,
        "tail_mean_rss_bytes": int(sum(tail_values) / len(tail_values)),
        "tail_max_rss_bytes": max(tail_values),
        "tail_growth_bytes": tail_growth,
        "tail_growth_ratio": growth_ratio,
        "tail_slope_bytes_per_second": linear_slope(points),
        "stable": growth_ratio <= max_tail_growth_ratio,
        "threshold_tail_growth_ratio": max_tail_growth_ratio,
    }


def read_cgroup_memory_events(pid: int) -> dict[str, int]:
    try:
        cgroup_lines = Path(f"/proc/{pid}/cgroup").read_text(encoding="utf-8").splitlines()
    except OSError:
        return {}
    relative = None
    for line in cgroup_lines:
        parts = line.split(":", 2)
        if len(parts) == 3 and parts[0] == "0":
            relative = parts[2].lstrip("/")
            break
    if relative is None:
        return {}
    path = Path("/sys/fs/cgroup") / relative / "memory.events"
    try:
        values = {}
        for line in path.read_text(encoding="utf-8").splitlines():
            key, value = line.split()
            values[key] = int(value)
        return values
    except (OSError, ValueError):
        return {}


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
