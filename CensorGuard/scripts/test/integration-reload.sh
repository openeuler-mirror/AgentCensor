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
runtime_dir=/tmp/censorguard-reload
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
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/audit.log"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  lab-policy:
    rules:
      - exec deny /usr/bin/id
domains:
  - name: lab-agent
    group: lab-policy
YAML

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 9 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

# ALLOW auditing is a runtime switch now: enable global exec allow auditing.
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    set --audit-exec on >"${runtime_dir}/set-audit.log"

"${repo_dir}/target/debug/censorguard-audit" launch \
    --socket "${runtime_dir}/events.sock" >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

set +e
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/before.log" 2>&1
before_rc=$?
set -e
if [[ ${before_rc} -eq 0 ]]; then
    echo "pre-reload id unexpectedly succeeded" >&2
    exit 1
fi

printf 'rules: []\n' | \
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    reload --group lab-policy --stdin >"${runtime_dir}/reload.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/after.log" 2>&1

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/status.log"

grep -q '"reload_gen": 1' "${runtime_dir}/status.log"
grep -q '"version": 2' "${runtime_dir}/reload.log"

wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=

grep -q 'DENY.*/usr/bin/id' "${runtime_dir}/audit.log"
grep -q 'ALLOW.*/usr/bin/id' "${runtime_dir}/audit.log"

echo "atomic reload integration passed: before_rc=${before_rc}, after_rc=0, version=2"
