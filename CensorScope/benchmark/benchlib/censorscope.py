from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .process import command_output, terminate_process_group


class CensorScopeError(RuntimeError):
    pass


@dataclass
class CensorScope:
    daemon_path: Path
    ctl_path: Path
    run_dir: Path
    level: str
    writer: dict[str, Any]
    daemon: subprocess.Popen[str] | None = None
    config_path: Path | None = None
    socket_path: Path | None = None
    database_path: Path | None = None
    log_path: Path | None = None
    trace_id: int | None = None

    def prepare(self) -> None:
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.config_path = self.run_dir / "censorscoped.conf"
        self.socket_path = self.run_dir / "censorscoped.sock"
        self.database_path = self.run_dir / "censorscope.sqlite"
        self.log_path = self.run_dir / "censorscoped.log"
        patch_path = self.run_dir / "operator.patch.toml"
        patch_path.write_text(self._patch_text(), encoding="utf-8")
        command = [
            str(self.daemon_path),
            "init",
            "--output",
            str(self.config_path),
            "--force",
            "--patch",
            str(patch_path),
        ]
        result = command_output(command)
        if result.returncode != 0:
            raise CensorScopeError(self._format_failure("init", result))

    def start(self) -> None:
        if not self.config_path:
            raise CensorScopeError("CensorScope configuration was not prepared")
        log = self.log_path.open("w", encoding="utf-8")
        self.daemon = subprocess.Popen(
            [str(self.daemon_path), "--config", str(self.config_path), "--level", self.level, "run"],
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
        log.close()
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.daemon.poll() is not None:
                raise CensorScopeError(f"daemon exited during startup; see {self.log_path}")
            result = self.ctl(["doctor"])
            if result.returncode == 0:
                return
            time.sleep(0.2)
        raise CensorScopeError(f"daemon did not become ready; see {self.log_path}")

    def ctl(self, args: list[str], timeout: float = 30.0) -> subprocess.CompletedProcess[str]:
        if not self.config_path:
            raise CensorScopeError("CensorScope configuration was not prepared")
        return command_output(
            [str(self.ctl_path), "--config", str(self.config_path), "--json", *args], timeout=timeout
        )

    def track(self, pid: int, name: str) -> int:
        result = self.ctl(["track-add", "--root-pid", str(pid), "--name", name])
        if result.returncode != 0:
            raise CensorScopeError(self._format_failure("track-add", result))
        try:
            reply = json.loads(result.stdout)
            trace_id = int(reply["trace_id"])
        except (ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
            raise CensorScopeError(f"cannot parse track-add response: {result.stdout!r}") from error
        self.trace_id = trace_id
        return trace_id

    def export(self, output_dir: Path) -> dict[str, Any]:
        if self.trace_id is None or self.database_path is None:
            return {"snapshot": False, "otlp_json": False}
        output_dir.mkdir(parents=True, exist_ok=True)
        result: dict[str, Any] = {}
        sessions = self.query("SELECT session_id FROM sessions WHERE session_id IS NOT NULL LIMIT 1")
        if sessions:
            started = time.monotonic()
            snapshot_path = output_dir / "snapshot.json"
            command = [
                "export",
                "--trace-id",
                str(self.trace_id),
                "--session-id",
                str(sessions[0][0]),
                "--out-path",
                str(snapshot_path),
                "--full",
            ]
            snapshot = self.ctl(command, timeout=120)
            result["snapshot"] = snapshot.returncode == 0
            result["snapshot_seconds"] = time.monotonic() - started
            result["snapshot_bytes"] = snapshot_path.stat().st_size if snapshot_path.exists() else 0
            if snapshot.returncode != 0:
                result["snapshot_error"] = snapshot.stderr.strip() or snapshot.stdout.strip()
        else:
            result["snapshot"] = False
            result["snapshot_error"] = "no session row"
        otlp_path = output_dir / "trace.otlp.json"
        started = time.monotonic()
        otlp = self.ctl(
            [
                "export",
                "--trace-id",
                str(self.trace_id),
                "--format",
                "otlp-json",
                "--out-path",
                str(otlp_path),
            ],
            timeout=120,
        )
        result["otlp_json"] = otlp.returncode == 0
        result["otlp_json_seconds"] = time.monotonic() - started
        result["otlp_json_bytes"] = otlp_path.stat().st_size if otlp_path.exists() else 0
        if otlp.returncode != 0:
            result["otlp_json_error"] = otlp.stderr.strip() or otlp.stdout.strip()
        return result

    def query(self, sql: str) -> list[tuple[Any, ...]]:
        if self.database_path is None:
            return []
        if not self.database_path.exists():
            return []
        try:
            with sqlite3.connect(f"file:{self.database_path}?mode=ro", uri=True, timeout=30) as connection:
                return [tuple(row) for row in connection.execute(sql)]
        except sqlite3.Error:
            return []

    def stats(self) -> dict[str, Any]:
        if self.database_path is None:
            return {}
        queries = {
            "events": "SELECT COUNT(*) FROM events",
            "payload_segments": "SELECT COUNT(*) FROM payload_segments",
            "payload_bytes": "SELECT COALESCE(SUM(captured_size), 0) FROM payload_segments",
            "sessions": "SELECT COUNT(*) FROM sessions",
            "call_spans": "SELECT COUNT(*) FROM call_spans",
            "semantic_actions": "SELECT COUNT(*) FROM semantic_actions",
            "semantic_links": "SELECT COUNT(*) FROM semantic_action_links",
            "diagnostics": "SELECT COUNT(*) FROM diagnostics",
            "loss_diagnostics": "SELECT COALESCE(SUM(dropped), 0) FROM diagnostics WHERE dropped > 0",
        }
        values: dict[str, Any] = {key: (rows[0][0] if (rows := self.query(sql)) else None) for key, sql in queries.items()}
        values["diagnostics_by_collector"] = [
            {"collector": row[0], "rows": row[1], "dropped": row[2]}
            for row in self.query(
                "SELECT collector, COUNT(*), COALESCE(SUM(dropped), 0) "
                "FROM diagnostics GROUP BY collector ORDER BY collector"
            )
        ]
        values["database_bytes"] = self.database_path.stat().st_size if self.database_path.exists() else 0
        wal = Path(f"{self.database_path}-wal")
        values["wal_bytes"] = wal.stat().st_size if wal.exists() else 0
        spool = self.database_path.with_suffix(".spool")
        values["spool_bytes"] = sum(path.stat().st_size for path in spool.glob("**/*") if path.is_file()) if spool.exists() else 0
        return values

    def stop(self) -> int | None:
        if self.daemon is None:
            return None
        returncode = terminate_process_group(self.daemon)
        self.daemon = None
        return returncode

    def _patch_text(self) -> str:
        return f'''[daemon]
socket_path = "{self.socket_path}"
pid_file = "{self.run_dir / "censorscoped.pid"}"
log_path = "{self.log_path}"
startup_wait_ms = 5000
shutdown_wait_ms = 10000
supervision_poll_interval_ms = 50

[storage]
path = "{self.database_path}"
busy_timeout_ms = 5000

[ebpf]
enabled = "auto"
tls_dynamic_loading = true

[external_events]
socket_path = "{self.run_dir / 'external-events.sock'}"

[writer]
batch_items = {int(self.writer.get("batch_items", 2048))}
idle_timeout_ms = {int(self.writer.get("idle_timeout_ms", 8))}
checkpoint_interval_secs = {int(self.writer.get("checkpoint_interval_secs", 60))}
queue_cap_items = {int(self.writer.get("queue_cap_items", 65536))}
queue_budget_bytes = {int(self.writer.get("queue_budget_bytes", 268435456))}
spool_max_bytes = {int(self.writer.get("spool_max_bytes", 4294967296))}
'''

    @staticmethod
    def _format_failure(label: str, result: subprocess.CompletedProcess[str]) -> str:
        detail = result.stderr.strip() or result.stdout.strip()
        return f"{label} failed with exit code {result.returncode}: {detail}"
