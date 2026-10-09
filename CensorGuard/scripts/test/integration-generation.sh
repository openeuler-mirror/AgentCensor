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
runtime_dir=/tmp/censorguard-generation
daemon_pid=
victim_pid=

cleanup() {
    [[ -n ${victim_pid} ]] && kill "${victim_pid}" 2>/dev/null || true
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

for tool in bpftool python3; do
    command -v "${tool}" >/dev/null
done

install -d -m 0700 "${runtime_dir}"
printf 'generation-safe\n' >"${runtime_dir}/protected.txt"
cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  guarded:
    rules:
      - file deny ${runtime_dir}/protected.txt [read]
domains:
  - name: generation-domain
    group: guarded
YAML
unlink "${runtime_dir}/start" 2>/dev/null || true
: >"${runtime_dir}/daemon.log"

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/config.yaml" \
    --duration 4 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

/bin/bash -c '
while [[ ! -e /tmp/censorguard-generation/start ]]; do
    /usr/bin/sleep 0.01
done
/usr/bin/cat /tmp/censorguard-generation/protected.txt
' >"${runtime_dir}/victim.log" 2>&1 &
victim_pid=$!
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    attach --pid "${victim_pid}" --domain generation-domain >"${runtime_dir}/attach.log"

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
mapfile -t pid_bytes < <(python3 - "${victim_pid}" <<'PY'
import sys

for byte in int(sys.argv[1]).to_bytes(4, "little"):
    print(f"{byte:02x}")
PY
)
bpftool map update id "${start_map_id}" key hex "${pid_bytes[@]}" \
    value hex 00 00 00 00 00 00 00 00

touch "${runtime_dir}/start"
wait "${victim_pid}"
victim_pid=
grep -q '^generation-safe$' "${runtime_dir}/victim.log"
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status >"${runtime_dir}/status.log"
grep -q '"tracked": 0' "${runtime_dir}/status.log"

wait "${daemon_pid}"
daemon_pid=
echo "PID generation integration passed: stale starttime mapping was rejected and removed"
