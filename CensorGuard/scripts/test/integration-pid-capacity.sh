#!/usr/bin/env bash
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/lib-cli.sh"

# Failure checkpoint: report script/line and dump the daemon log tail.
on_err() {
    local rc=$?
    echo "[FAIL] $(basename "$0"):${1} rc=${rc}" >&2
    for d in "${runtime_dir:-}" "${lab_dir:-}"; do
        if [[ -n ${d} && -f ${d}/daemon.log ]]; then
            echo "--- ${d}/daemon.log (last 30 lines) ---" >&2
            tail -30 "${d}/daemon.log" >&2
        fi
    done
}
trap 'on_err "${LINENO}"' ERR
set -euo pipefail

# 断言依赖系统工具的英文输出 (id(1) strerror(3) 等会被 locale 本地化),
# 统一固定 C locale, 避免 zh_CN 环境下 grep 误判。
export LC_ALL=C

# Failure checkpoint: report script/line and dump the daemon log tail.
on_err() {
    local rc=$?
    echo "[FAIL] $(basename "$0"):${1} rc=${rc}" >&2
    for d in "${runtime_dir:-}" "${lab_dir:-}"; do
        if [[ -n ${d} && -f ${d}/daemon.log ]]; then
            echo "--- ${d}/daemon.log (last 30 lines) ---" >&2
            tail -30 "${d}/daemon.log" >&2
        fi
    done
}
trap 'on_err "${LINENO}"' ERR

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-pid-capacity
daemon_pid=
probe_pid=
fake_count=4095
fake_base=1879048192

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
printf 'capacity-protected\n' >"${runtime_dir}/protected.txt"
cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  guarded:
    rules:
      - file deny ${runtime_dir}/protected.txt [read]
domains:
  - name: capacity-domain
    group: guarded
YAML
for sync_file in start ready release; do
    unlink "${runtime_dir}/${sync_file}" 2>/dev/null || true
done
: >"${runtime_dir}/daemon.log"

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 20 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${runtime_dir}/pid-churn-probe" "${runtime_dir}/start" \
    "${runtime_dir}/ready" "${runtime_dir}/release" \
    "${runtime_dir}/protected.txt" 1 &
probe_pid=$!
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    attach --pid "${probe_pid}" --domain capacity-domain >"${runtime_dir}/attach.log"

bpftool -j map show >"${runtime_dir}/maps.json"
tracked_map_id=$(python3 - "${runtime_dir}/maps.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    maps = json.load(stream)
candidates = [
    item for item in maps
    if item.get("name") == "tracked_pids" and item.get("max_entries") == 4096
]
if not candidates:
    raise SystemExit("cannot find Censorguard tracked_pids map")
print(max(item["id"] for item in candidates))
PY
)
python3 - "${runtime_dir}/fill.batch" "${runtime_dir}/delete.batch" \
    "${tracked_map_id}" "${fake_base}" "${fake_count}" <<'PY'
import sys

fill_path, delete_path, map_id, base, count = sys.argv[1:]
base = int(base)
count = int(count)
domain = 0xFEE1DEAD

def hex_bytes(value, size):
    return " ".join(f"{byte:02x}" for byte in value.to_bytes(size, "little"))

with open(fill_path, "w", encoding="ascii") as fill, \
     open(delete_path, "w", encoding="ascii") as delete:
    # tracked_pids now stores only the runtime scope ID; policy-slot binding lives in
    # scope_policies and is intentionally decoupled so multiple scopes can share a group.
    value = hex_bytes(domain, 8)
    for offset in range(count):
        key = hex_bytes(base + offset, 4)
        fill.write(f"map update id {map_id} key hex {key} value hex {value}\n")
        delete.write(f"map delete id {map_id} key hex {key}\n")
PY
bpftool batch file "${runtime_dir}/fill.batch" >"${runtime_dir}/fill.log"

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status --json >"${runtime_dir}/full.json"
python3 - "${runtime_dir}/full.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    status = json.load(stream)
assert status["tracked"] == status["pid_tracked_capacity"] == 4096, status
assert status["pid_pending"] == 0, status
assert status["version"] == 1 and status["active_bank"] == 0, status
PY

touch "${runtime_dir}/start"
for _ in $(seq 1 1200); do
    [[ -s ${runtime_dir}/ready ]] && break
    /usr/bin/sleep 0.025
done
grep -q ' 1$' "${runtime_dir}/ready"
child_pid=$(awk 'NR == 1 { print $1 }' "${runtime_dir}/ready")
kill -0 "${child_pid}"

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status --json >"${runtime_dir}/degraded.json"
python3 - "${runtime_dir}/degraded.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    status = json.load(stream)
assert status["ok"] is True, status
assert status["tracked"] == 4096, status
assert status["pid_pending"] == 1, status
assert status["pid_tracked_update_failures"] >= 1, status
assert status["pid_pending_fallbacks"] >= 1, status
assert status["pid_pending_update_failures"] == 0, status
assert status["version"] == 1 and status["active_bank"] == 0, status
PY
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status --json >"${runtime_dir}/tree.json"
grep -q "\"pid\": ${child_pid}" "${runtime_dir}/tree.json"

# Unattach must remove the root and pending-backed child immediately.
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    unattach --pid "${probe_pid}" >"${runtime_dir}/untrack.log"
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status --json >"${runtime_dir}/draining.json"
python3 - "${runtime_dir}/draining.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    status = json.load(stream)
assert not any(item["name"] == "capacity-domain" for item in status.get("domains", [])), status
assert status["pid_pending"] == 0, status
PY

bpftool batch file "${runtime_dir}/delete.batch" >"${runtime_dir}/delete.log"
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status --json >"${runtime_dir}/after-delete.json"
python3 - "${runtime_dir}/after-delete.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    status = json.load(stream)
assert status["tracked"] == 0, status
assert status["pid_pending"] == 0, status
assert not any(item["name"] == "capacity-domain" for item in status.get("domains", [])), status
PY

touch "${runtime_dir}/release"
wait "${probe_pid}"
probe_pid=
for _ in $(seq 1 240); do
    "${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
        status --json >"${runtime_dir}/final.json"
    if grep -q '"pid_pending": 0' "${runtime_dir}/final.json" && \
        ! grep -q '"name": "capacity-domain"' "${runtime_dir}/final.json"; then
        break
    fi
    /usr/bin/sleep 0.05
done
grep -q '"tracked": 0' "${runtime_dir}/final.json"
grep -q '"pid_pending": 0' "${runtime_dir}/final.json"
if grep -q '"name": "capacity-domain"' "${runtime_dir}/final.json"; then
    echo "pending-backed draining domain was not reclaimed" >&2
    exit 1
fi

wait "${daemon_pid}"
daemon_pid=
echo "PID capacity integration passed: full tracked map fell back to pending without bypass"
