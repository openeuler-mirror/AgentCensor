#!/usr/bin/env bash

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
runtime_dir=/tmp/censorguard-spawn
daemon_pid=
audit_pid=

cleanup() {
    for pid in "${audit_pid}" "${daemon_pid}"; do
        if [[ -n ${pid} ]]; then
            kill "${pid}" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  lab-policy:
    rules:
      - exec deny /usr/bin/id
domains:
  - name: lab-agent
    group: lab-policy
YAML
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/audit.log"
: >"${runtime_dir}/spawn.log"
: >"${runtime_dir}/allowed.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 5 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    if grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
        break
    fi
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguard-audit" launch \
    --socket "${runtime_dir}/events.sock" >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

"${repo_dir}/target/debug/censorguardctl" \
    --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/printf '%s\n' 'hello world' \
    >"${runtime_dir}/allowed.log" 2>&1
grep -qx 'hello world' "${runtime_dir}/allowed.log"

set +e
"${repo_dir}/target/debug/censorguardctl" \
    --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/spawn.log" 2>&1
spawn_rc=$?
set -e

if [[ ${spawn_rc} -eq 0 ]]; then
    echo "expected /usr/bin/id to be denied" >&2
    cat "${runtime_dir}/spawn.log" >&2
    exit 1
fi

wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=

if ! grep -q 'DENY' "${runtime_dir}/audit.log"; then
    echo "audit stream did not contain an exec DENY" >&2
    cat "${runtime_dir}/audit.log" >&2
    exit 1
fi
if grep -Eq 'EXEC +exec +[0-9]+ +DENY +[^ ]+ +bash +/usr/bin/id' \
    "${runtime_dir}/audit.log"; then
    echo "spawn unexpectedly passed through bash" >&2
    cat "${runtime_dir}/audit.log" >&2
    exit 1
fi

echo "ctl spawn integration passed: id rc=${spawn_rc}"
grep 'DENY' "${runtime_dir}/audit.log" | tail -n 1
