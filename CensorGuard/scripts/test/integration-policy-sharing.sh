#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-policy-sharing
daemon_pid=
probe_pids=()

cleanup() {
    for pid in "${probe_pids[@]:-}"; do
        kill "${pid}" 2>/dev/null || true
        wait "${pid}" 2>/dev/null || true
    done
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

for tool in bpftool python3; do
    command -v "${tool}" >/dev/null
done

install -d -m 0700 "${runtime_dir}"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  lab-policy:
    rules:
      - exec deny /usr/bin/id
domains:
  - name: lab-agent
    group: lab-policy
  - name: lab-agent-2
    group: lab-policy
YAML
: >"${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --launch-sock "${runtime_dir}/launch.sock" \
    --duration 10 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

# Keep two existing processes alive while they are attached.  The test is about runtime scopes,
# not the launcher path; using attach here also verifies that domains sharing a policy group get
# separate scope IDs with the same policy-slot binding.
/usr/bin/sleep 30 &
probe_pids+=("$!")
/usr/bin/sleep 30 &
probe_pids+=("$!")
probe_one=${probe_pids[0]}
probe_two=${probe_pids[1]}

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    attach --pid "${probe_one}" --domain lab-agent >"${runtime_dir}/attach-one.json"
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    attach --pid "${probe_two}" --domain lab-agent-2 >"${runtime_dir}/attach-two.json"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/status.json"

python3 - "${runtime_dir}/status.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    response = json.load(stream)

domains = {item["name"]: item for item in response.get("domains", [])}
required = ["lab-agent", "lab-agent-2"]
missing = [name for name in required if name not in domains]
if missing:
    raise SystemExit(f"attached domains missing from status: {missing}; got {sorted(domains)}")
first, second = (domains[name] for name in required)
if first["id"] == second["id"]:
    raise SystemExit(f"domains unexpectedly share scope id: {first['id']}")
if first["slot"] != second["slot"]:
    raise SystemExit(
        f"domains mapped to different policy slots: {first['slot']} != {second['slot']}"
    )
print(
    f"attached scopes {first['id']} and {second['id']} share logical policy slot "
    f"{first['slot']} (group {first['group']})"
)
PY

bpftool -j map show >"${runtime_dir}/maps.json"
outer_id=$(python3 - "${runtime_dir}/maps.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    maps = json.load(stream)
candidates = [item for item in maps if item.get("name") == "dom_cmd_ino" and item.get("max_entries") == 128]
if not candidates:
    raise SystemExit("cannot find Censorguard dom_cmd_ino outer map")
print(max(item["id"] for item in candidates))
PY
)
bpftool -j map dump id "${outer_id}" >"${runtime_dir}/outer.json"
python3 - "${runtime_dir}/status.json" "${runtime_dir}/outer.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    status = json.load(stream)
with open(sys.argv[2], encoding="utf-8") as stream:
    entries = json.load(stream)

def number(raw):
    return int.from_bytes(bytes(int(byte, 16) for byte in raw), "little")

domains = {item["name"]: item for item in status.get("domains", [])}
logical_slot = domains["lab-agent"]["slot"]
active_bank = status["active_bank"] & 1
physical_slot = active_bank * 64 + logical_slot
slots = {number(item["key"]): number(item["value"]) for item in entries}
inner_id = slots.get(physical_slot)
if inner_id is None or inner_id == 0:
    raise SystemExit(
        f"dom_cmd_ino has no inner map for active bank slot {physical_slot}; "
        f"logical={logical_slot} active_bank={active_bank} entries={sorted(slots)}"
    )
print(
    f"policy sharing integration passed: scopes lab-agent/lab-agent-2 -> "
    f"logical slot {logical_slot}, physical slot {physical_slot}, inner map id {inner_id}"
)
PY

wait "${daemon_pid}"
daemon_pid=
