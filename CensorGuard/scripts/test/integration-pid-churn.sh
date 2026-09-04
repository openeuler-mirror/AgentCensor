#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-pid-churn
daemon_pid=
probe_pid=
child_count=256

cleanup() {
    [[ -n ${probe_pid} ]] && kill "${probe_pid}" 2>/dev/null || true
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

for tool in bpftool python3; do
    command -v "${tool}" >/dev/null
done

install -d -m 0700 "${runtime_dir}"
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/pid_churn_probe.c" \
    -o "${runtime_dir}/pid-churn-probe"
printf 'pid-churn-protected\n' >"${runtime_dir}/protected.txt"
cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  guarded:
    rules:
      - file deny ${runtime_dir}/protected.txt [read]
domains:
  - name: churn-domain
    group: guarded
YAML
for sync_file in start ready release; do
    unlink "${runtime_dir}/${sync_file}" 2>/dev/null || true
done
: >"${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 18 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${runtime_dir}/pid-churn-probe" "${runtime_dir}/start" \
    "${runtime_dir}/ready" "${runtime_dir}/release" \
    "${runtime_dir}/protected.txt" "${child_count}" &
probe_pid=$!
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    attach --pid "${probe_pid}" --domain churn-domain >"${runtime_dir}/attach.log"
touch "${runtime_dir}/start"

for _ in $(seq 1 400); do
    if [[ -f ${runtime_dir}/ready ]] && \
        [[ $(wc -l <"${runtime_dir}/ready") -eq ${child_count} ]]; then
        break
    fi
    /usr/bin/sleep 0.025
done
[[ $(wc -l <"${runtime_dir}/ready") -eq ${child_count} ]]
if grep -q ' 0$' "${runtime_dir}/ready"; then
    echo "at least one concurrent child escaped file enforcement" >&2
    exit 1
fi

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/status.json"
python3 - "${runtime_dir}/status.json" "${child_count}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    status = json.load(stream)
children = int(sys.argv[2])
expected = children + 1
assert status["ok"] is True, status
assert status["tracked"] == expected, status
assert status["pid_generations"] == expected, status
assert status["pid_pending"] == 0, status
assert status["pid_tracked_capacity"] == 4096, status
assert status["pid_pending_capacity"] == 4096, status
assert status["pid_start_update_failures"] == 0, status
assert status["pid_tracked_update_failures"] == 0, status
assert status["pid_pending_update_failures"] == 0, status
PY

bpftool -j map show >"${runtime_dir}/maps.json"
start_map_id=$(python3 - "${runtime_dir}/maps.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    maps = json.load(stream)
candidates = [
    item for item in maps
    if item.get("name") == "pid_start_times" and item.get("max_entries") == 4096
]
if not candidates:
    raise SystemExit("cannot find Censorguard pid_start_times map")
print(max(item["id"] for item in candidates))
PY
)
bpftool -j map dump id "${start_map_id}" >"${runtime_dir}/starts.json"
python3 - "${runtime_dir}/ready" "${probe_pid}" "${runtime_dir}/starts.json" <<'PY'
import json
import sys

def number(raw):
    return int.from_bytes(bytes(int(byte, 16) for byte in raw), "little")

with open(sys.argv[1], encoding="utf-8") as stream:
    pids = {int(line.split()[0]) for line in stream if line.strip()}
pids.add(int(sys.argv[2]))
with open(sys.argv[3], encoding="utf-8") as stream:
    entries = json.load(stream)
starts = {number(item["key"]): number(item["value"]) for item in entries}
for pid in pids:
    with open(f"/proc/{pid}/stat", encoding="utf-8") as stream:
        suffix = stream.read().rsplit(") ", 1)[1].split()
    proc_start = int(suffix[19])
    if starts.get(pid) != proc_start:
        raise SystemExit(
            f"generation mismatch for pid {pid}: map={starts.get(pid)} proc={proc_start}"
        )
print(f"verified {len(pids)} concurrent PID generations")
PY

touch "${runtime_dir}/release"
wait "${probe_pid}"
probe_pid=

for _ in $(seq 1 240); do
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        status >"${runtime_dir}/final.json"
    if grep -q '"tracked": 0' "${runtime_dir}/final.json" && \
        ! grep -q '"name": "churn-domain"' "${runtime_dir}/final.json"; then
        break
    fi
    /usr/bin/sleep 0.05
done
grep -q '"tracked": 0' "${runtime_dir}/final.json"
if grep -q '"name": "churn-domain"' "${runtime_dir}/final.json"; then
    echo "churn domain was not reclaimed" >&2
    exit 1
fi

wait "${daemon_pid}"
daemon_pid=
echo "PID churn integration passed: ${child_count} concurrent children denied with matching generations"
