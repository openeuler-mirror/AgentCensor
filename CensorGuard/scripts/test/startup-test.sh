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
set -Eeuo pipefail

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

# Minimal 0.4.0 smoke test. Run as root on a host with BPF LSM enabled.
repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=$(mktemp -d /tmp/censorguard-smoke.XXXXXX)
cleanup() { kill "${daemon_pid:-}" "${audit_pid:-}" 2>/dev/null || true; rm -rf -- "${runtime_dir}"; }
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"

"${repo_dir}/target/release/censorguardd" launch \
  --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
  --ctl-sock "${runtime_dir}/ctl.sock" \
  --event-sock "${runtime_dir}/events.sock" \
  --launch-sock "${runtime_dir}/launch.sock" \
  --duration 20 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 1200); do grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break; sleep 0.05; done
if ! grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
  echo "daemon did not become READY; daemon.log follows:" >&2
  cat "${runtime_dir}/daemon.log" >&2
  exit 1
fi

"${repo_dir}/target/release/censorguard-audit" launch --socket "${runtime_dir}/events.sock" >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

# Apply the only canonical policy entrypoint, then attach this probe by PID.
sleep 0.1
"${repo_dir}/target/release/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
  --policy "${repo_dir}/config/base.yaml" --pid "$$" >/dev/null

if cat /etc/shadow >/dev/null 2>"${runtime_dir}/read.err"; then
  echo 'expected /etc/shadow read to be denied' >&2
  exit 1
fi
grep -q 'DENY' "${runtime_dir}/audit.log"
echo "Censorguard 0.4.0 startup smoke test passed"
