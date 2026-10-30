#!/usr/bin/env python3
"""Run the modular CensorScope benchmark suite."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

from benchlib.censorscope import CensorScope, CensorScopeError
from benchlib.config import (
    ConfigError,
    execution_mode,
    load_scenario,
    module_cases,
    module_requires_export,
    resolve_executable,
    selected_modules,
)
from benchlib.metrics import MemorySampler, memory_summary, read_cgroup_memory_events, write_json
from benchlib.process import command_output, continue_process, spawn_paused_exec, terminate_process_group


ROOT = Path(__file__).resolve().parent


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scenario", type=Path, default=ROOT / "scenarios" / "deepseek-harness.toml")
    parser.add_argument("--module", default="all", help="all or comma-separated module names")
    parser.add_argument(
        "--mode",
        choices=("auto", "manual", "native"),
        default="auto",
        help="dsh mode for e2e; non-e2e modules always use native workload execution",
    )
    parser.add_argument("--dsh-path", help="dsh executable; defaults to shutil.which('dsh')")
    parser.add_argument("--daemon-path", help="censorscoped executable")
    parser.add_argument("--ctl-path", help="censorscopectl executable")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "results")
    parser.add_argument("--keep-runtime", action="store_true", help="retain temporary databases and spool files")
    parser.add_argument("--dry-run", action="store_true", help="print planned commands without starting processes")
    return parser.parse_args()


def repo_binary(explicit: str | None, release_name: str) -> Path:
    if explicit:
        path = Path(explicit).expanduser().resolve()
    else:
        path = (ROOT.parent / "target" / "release" / release_name).resolve()
    if not path.is_file():
        raise ConfigError(f"cannot find {release_name}; build release binaries or pass an explicit path")
    return path


def workload_command(case: dict[str, Any], output: Path) -> list[str]:
    if case["module"] == "preflight":
        return []
    workload = case["workload"]
    helper = ROOT / "workloads" / "system_load.py"
    command = [
        sys.executable,
        str(helper),
        "--profile",
        str(case["module"]),
        "--output",
        str(output),
        "--iterations",
        str(int(workload.get("iterations", 1))),
        "--duration-seconds",
        str(float(workload.get("duration_seconds", 0))),
        "--file-operations",
        str(int(workload.get("file_operations", 10))),
        "--payload-bytes",
        str(int(workload.get("payload_bytes", 4096))),
        "--children",
        str(int(workload.get("children", 1))),
        "--concurrency",
        str(int(workload.get("concurrency", 1))),
        "--seed",
        str(int(workload.get("seed", scenario_seed(case)))),
    ]
    return command


def scenario_seed(case: dict[str, Any]) -> int:
    return int(case.get("seed", 42))


def prompt_for(case: dict[str, Any], command: list[str]) -> str:
    prefix = case.get("prompt_prefix", "")
    instruction = (
        "Run this benchmark command exactly once using the shell tool, wait for it to finish, "
        "and then reply with its JSON result. Do not run it more than once: "
        f"{shlex.join(command)}"
    )
    return f"{prefix} {instruction}".strip()


def wait_for_marker(marker: Path, process: subprocess.Popen[str] | None, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if marker.exists():
            try:
                return json.loads(marker.read_text(encoding="utf-8"))
            except json.JSONDecodeError as error:
                raise RuntimeError(f"workload marker is not valid JSON: {marker}") from error
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"benchmark process exited before workload completion with code {process.returncode}")
        time.sleep(0.25)
    raise TimeoutError(f"workload did not finish within {timeout:.1f}s: {marker}")


def start_control_probe(cs: CensorScope, interval: float) -> tuple[threading.Event, threading.Thread, list[dict[str, Any]]]:
    stop = threading.Event()
    samples: list[dict[str, Any]] = []

    def probe() -> None:
        commands = (["doctor"], ["trace-list"])
        index = 0
        while not stop.is_set():
            command = commands[index % len(commands)]
            index += 1
            started = time.monotonic()
            try:
                reply = cs.ctl(command, timeout=30)
                samples.append({
                    "command": command[0],
                    "seconds": time.monotonic() - started,
                    "returncode": reply.returncode,
                })
            except Exception as error:  # The main case records the failure details.
                samples.append({"command": command[0], "seconds": time.monotonic() - started, "error": str(error)})
            stop.wait(interval)

    thread = threading.Thread(target=probe, name="benchmark-control-probe", daemon=True)
    thread.start()
    return stop, thread, samples


def summarize_control_probe(samples: list[dict[str, Any]]) -> dict[str, Any]:
    durations = [float(sample["seconds"]) for sample in samples]
    failures = [sample for sample in samples if sample.get("returncode") not in (0, None) or "error" in sample]
    if not durations:
        return {"requests": 0, "failures": 0}
    ordered = sorted(durations)
    p95_index = min(len(ordered) - 1, int(len(ordered) * 0.95))
    return {
        "requests": len(durations),
        "failures": len(failures),
        "p50_seconds": statistics.median(durations),
        "p95_seconds": ordered[p95_index],
        "max_seconds": max(durations),
    }


def run_case(
    scenario: dict[str, Any],
    case: dict[str, Any],
    output_root: Path,
    mode: str,
    dsh_path: Path | None,
    daemon_path: Path,
    ctl_path: Path,
    keep_runtime: bool,
    dry_run: bool,
) -> dict[str, Any]:
    case_dir = output_root / case["case_id"]
    case_dir.mkdir(parents=True, exist_ok=True)
    runtime_dir = case_dir / "runtime"
    workload_dir = case_dir / "workload"
    workload_dir.mkdir(parents=True, exist_ok=True)
    command = workload_command(case, workload_dir)
    requires_export = module_requires_export(case["module"])
    dsh_args = [str(value) for value in scenario["agent"].get("args", ["--profile", "headless"])]
    dsh_argv = [str(dsh_path), *dsh_args, prompt_for(case, command)] if dsh_path and command else []
    agent_argv = command if mode == "native" else dsh_argv
    result: dict[str, Any] = {
        "case_id": case["case_id"],
        "module": case["module"],
        "repetition": case["repetition"],
        "mode": mode,
        "dsh_path": str(dsh_path) if dsh_path else None,
        "dsh_command": dsh_argv,
        "agent_command": agent_argv,
        "workload_command": command,
        "status": "failed",
    }

    def record_failure(phase: str, error: BaseException) -> None:
        message = f"{phase}: {error}"
        result.setdefault("errors", []).append(message)
        result.setdefault("error", message)
        result["status"] = "failed"

    if dry_run:
        result["status"] = "planned"
        result["planned_command"] = agent_argv
        write_json(case_dir / "summary.json", result)
        return result

    cs = CensorScope(
        daemon_path=daemon_path,
        ctl_path=ctl_path,
        run_dir=runtime_dir,
        level=str(scenario["censorscope"].get("level", "L3")),
        writer=dict(scenario["censorscope"].get("writer", {})),
    )
    agent_process: subprocess.Popen[str] | None = None
    sampler = MemorySampler(
        case_dir / "samples.csv",
        float(scenario["memory"].get("sample_interval_seconds", 1.0)),
    )
    cgroup_before: dict[str, int] = {}
    cgroup_after: dict[str, int] = {}
    control_stop: threading.Event | None = None
    control_thread: threading.Thread | None = None
    control_samples: list[dict[str, Any]] = []
    try:
        cs.prepare()
        cs.start()
        if cs.daemon is None:
            raise CensorScopeError("daemon process was not started")
        if cs.database_path is None:
            raise CensorScopeError("database path was not prepared")
        sampler.size_paths = {
            "database": cs.database_path,
            "wal": Path(f"{cs.database_path}-wal"),
            "spool": cs.database_path.with_suffix(".spool"),
        }
        sampler.add_root("daemon", cs.daemon.pid)
        sampler.start()
        cgroup_before = read_cgroup_memory_events(cs.daemon.pid)
        if case["module"] == "preflight":
            pass
        elif mode in ("auto", "native"):
            log_name = "dsh.log" if mode == "auto" else "workload.log"
            log = (case_dir / log_name).open("w", encoding="utf-8")
            env = os.environ.copy()
            if mode == "auto":
                session = f"censorscope-benchmark-{case['case_id']}-{os.getpid()}"
                env["CENSORSCOPE_SESSION_ID"] = session
                env["DSH_SESSION_ID"] = session
                env["CENSORSCOPE_BENCHMARK_CASE"] = case["case_id"]
            else:
                for key in (
                    "CENSORSCOPE_SESSION_ID",
                    "DSH_SESSION_ID",
                    "DSH_CENSORSCOPE_CALL_ID",
                    "AGENTCENSOR_SESSION_ID",
                ):
                    env.pop(key, None)
            agent_process = spawn_paused_exec(agent_argv, env, log, log)
            log.close()
            sampler.add_root("dsh" if mode == "auto" else "workload", agent_process.pid)
            cs.track(agent_process.pid, f"benchmark-{case['case_id']}")
            continue_process(agent_process)
        elif mode == "manual":
            print(f"\nManual module {case['case_id']}: start dsh with this command:\n  {shlex.join(dsh_argv)}")
            raw_pid = input("Enter the dsh PID after it is running: ").strip()
            dsh_pid = int(raw_pid)
            cs.track(dsh_pid, f"benchmark-{case['case_id']}")
            sampler.add_root("dsh", dsh_pid)
        if case["module"] == "control":
            control_stop, control_thread, control_samples = start_control_probe(
                cs, float(case.get("control_probe_interval_seconds", 0.2))
            )
        if case["module"] != "preflight":
            marker = workload_dir / "complete.json"
            timeout = float(case.get("timeout_seconds", 300))
            workload_result = wait_for_marker(marker, agent_process, timeout)
            result["workload"] = workload_result
            time.sleep(float(case.get("drain_seconds", 5)))
        else:
            time.sleep(float(case.get("drain_seconds", 1)))
        if agent_process is not None:
            cgroup_after = read_cgroup_memory_events(agent_process.pid)
            result["agent_exit_code"] = terminate_process_group(
                agent_process, float(case.get("stop_grace_seconds", 10))
            )
        result["trace_id"] = cs.trace_id
        result["status"] = "passed"
    except Exception as error:
        record_failure("run", error)
        if agent_process is not None:
            try:
                result["agent_exit_code"] = agent_process.poll()
            except Exception as poll_error:
                record_failure("agent status", poll_error)
    finally:
        if control_stop is not None:
            try:
                control_stop.set()
            except Exception as error:
                record_failure("control probe stop", error)
        if control_thread is not None:
            try:
                control_thread.join(timeout=5)
            except Exception as error:
                record_failure("control probe join", error)
        if case["module"] == "control":
            try:
                result["control_probe"] = summarize_control_probe(control_samples)
            except Exception as error:
                record_failure("control probe summary", error)
        try:
            sampler.stop()
        except Exception as error:
            record_failure("memory sampler stop", error)
        if agent_process is not None:
            try:
                if agent_process.poll() is None:
                    result["agent_exit_code"] = terminate_process_group(agent_process)
            except Exception as error:
                record_failure("agent cleanup", error)
        if cs.daemon is not None:
            try:
                cgroup_after = read_cgroup_memory_events(cs.daemon.pid)
            except Exception as error:
                record_failure("cgroup metrics", error)
        try:
            result["daemon_exit_code"] = cs.stop()
        except Exception as error:
            record_failure("daemon cleanup", error)
        try:
            result["stats"] = cs.stats()
        except Exception as error:
            record_failure("stats", error)
            result["stats"] = {}
        if requires_export:
            try:
                result["exports"] = cs.export(case_dir / "exports")
            except Exception as error:
                record_failure("export", error)
                result["exports"] = {"snapshot": False, "otlp_json": False, "error": str(error)}
        else:
            result["exports"] = {
                "skipped": True,
                "reason": "memory stability does not require full trace export"
                if case["module"] == "memory"
                else "readiness does not require trace export",
            }
        try:
            result["memory"] = memory_summary(
                sampler.samples,
                float(scenario["memory"].get("tail_fraction", 0.25)),
                float(scenario["memory"].get("max_tail_growth_ratio", 0.05)),
            )
        except Exception as error:
            record_failure("memory summary", error)
            result["memory"] = {"stable": False, "reason": str(error)}
        result["cgroup_memory_events_before"] = cgroup_before
        result["cgroup_memory_events_after"] = cgroup_after
        result["oom_detected"] = _oom_detected(result)
        if result.get("status") == "passed" and (result["oom_detected"] or not result["memory"].get("stable", False)):
            result["status"] = "failed"
            result["error"] = "memory stability or OOM criterion failed"
        if result.get("status") == "passed" and result.get("agent_exit_code") not in (None, 0):
            result["status"] = "failed"
            result["error"] = f"benchmark agent exited with code {result['agent_exit_code']}"
        if result.get("status") == "passed" and requires_export and not result.get("exports", {}).get("otlp_json", False):
            result["status"] = "failed"
            result["error"] = "OTLP export failed"
        if result.get("status") == "passed" and case["module"] != "preflight" and not result.get("stats", {}).get("events", 0):
            result["status"] = "failed"
            result["error"] = "no events were committed"
        if result.get("status") == "passed" and result.get("workload", {}).get("totals", {}).get("skipped_operations", 0):
            result["status"] = "failed"
            result["error"] = "workload skipped operations; check test permissions"
        try:
            write_json(case_dir / "summary.json", result)
        except Exception as error:
            record_failure("summary write", error)
            try:
                write_json(case_dir / "summary.json", result)
            except Exception:
                pass
        if not keep_runtime:
            try:
                _remove_runtime_payload(runtime_dir)
            except Exception as error:
                record_failure("runtime cleanup", error)
    return result


def _oom_detected(result: dict[str, Any]) -> bool:
    for key in ("agent_exit_code", "dsh_exit_code", "workload_exit_code", "daemon_exit_code"):
        code = result.get(key)
        if code in (137, -signal.SIGKILL):
            return True
    before = result.get("cgroup_memory_events_before", {})
    after = result.get("cgroup_memory_events_after", {})
    return any(after.get(key, 0) > before.get(key, 0) for key in ("oom", "oom_kill", "oom_group_kill"))


def _remove_runtime_payload(runtime_dir: Path) -> None:
    for name in ("censorscope.sqlite", "censorscope.sqlite-shm", "censorscope.sqlite-wal", "censorscope.spool"):
        path = runtime_dir / name
        if path.is_dir():
            shutil.rmtree(path, ignore_errors=True)
        elif path.exists():
            path.unlink(missing_ok=True)


def write_report(output_root: Path, scenario: dict[str, Any], results: list[dict[str, Any]]) -> None:
    aggregate = {
        "scenario": scenario.get("_meta", {}),
        "results": results,
        "passed": all(item["status"] in ("passed", "planned") for item in results),
    }
    write_json(output_root / "summary.json", aggregate)
    lines = [
        "# CensorScope benchmark result",
        "",
        f"Overall: **{'PASS' if aggregate['passed'] else 'FAIL'}**",
        "",
        "| Module | Status | Peak RSS | Tail growth | Events | Payload bytes | Loss diagnostics | Control p95 | OTLP sec |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for item in results:
        memory = item.get("memory", {})
        stats = item.get("stats", {})
        lines.append(
            f"| {item['module']} ({item['case_id']}) | {item['status']} | {memory.get('peak_rss_bytes', '')} "
            f"| {memory.get('tail_growth_ratio', '')} | {stats.get('events', '')} "
            f"| {stats.get('payload_bytes', '')} | {stats.get('loss_diagnostics', '')} "
            f"| {item.get('control_probe', {}).get('p95_seconds', '')} "
            f"| {item.get('exports', {}).get('otlp_json_seconds', '')} |"
        )
    lines.extend(["", "The raw samples are in each case's `samples.csv`; command and environment details are in `summary.json`."])
    (output_root / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    try:
        scenario = load_scenario(args.scenario.resolve())
        modules = selected_modules(args.module)
        modes = {module: execution_mode(args.mode, module) for module in modules}
        needs_dsh = "e2e" in modules
        dsh_path = resolve_executable(args.dsh_path, "dsh", "deepseek-harness (dsh)") if needs_dsh else None
        daemon_path = repo_binary(args.daemon_path, "censorscoped")
        ctl_path = repo_binary(args.ctl_path, "censorscopectl")
    except ConfigError as error:
        print(f"benchmark configuration error: {error}", file=sys.stderr)
        return 2
    output_root = args.output_dir.resolve()
    output_root.mkdir(parents=True, exist_ok=True)
    try:
        dsh_version = command_output([str(dsh_path), "--version"], timeout=15) if dsh_path else None
        write_json(output_root / "plan.json", {
            "scenario": scenario.get("_meta", {}),
            "modules": modules,
            "mode": args.mode,
            "module_modes": modes,
            "dsh_path": str(dsh_path) if dsh_path else None,
            "dsh_version": (dsh_version.stdout.strip() or dsh_version.stderr.strip()) if dsh_version else None,
            "daemon_path": str(daemon_path),
            "ctl_path": str(ctl_path),
        })
    except Exception as error:
        print(f"benchmark setup failed: {error}", file=sys.stderr)
        return 2
    results: list[dict[str, Any]] = []
    for module in modules:
        for case in module_cases(scenario, module):
            print(f"[{case['case_id']}] starting")
            mode = modes[module]
            try:
                result = run_case(
                    scenario,
                    case,
                    output_root,
                    mode,
                    dsh_path,
                    daemon_path,
                    ctl_path,
                    args.keep_runtime,
                    args.dry_run,
                )
            except Exception as error:
                result = {
                    "case_id": case["case_id"],
                    "module": case["module"],
                    "repetition": case["repetition"],
                    "mode": mode,
                    "status": "failed",
                    "error": f"case: {error}",
                }
                try:
                    write_json(output_root / case["case_id"] / "summary.json", result)
                except Exception:
                    pass
            results.append(result)
            print(f"[{case['case_id']}] {result['status']}")
    try:
        write_report(output_root, scenario, results)
    except Exception as error:
        print(f"benchmark report failed: {error}", file=sys.stderr)
        return 1
    print(f"results: {output_root / 'summary.md'}")
    return 0 if all(item["status"] in ("passed", "planned") for item in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
