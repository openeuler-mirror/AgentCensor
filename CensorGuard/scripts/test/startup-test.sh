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

# Minimal 0.5.0 smoke test. Run as root on a host with BPF LSM enabled.
repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=$(mktemp -d /tmp/censorguard-smoke.XXXXXX)
cleanup() { kill "${daemon_pid:-}" "${audit_pid:-}" 2>/dev/null || true; rm -rf -- "${runtime_dir}"; }
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"

cat >"${runtime_dir}/base.yaml" <<EOF
daemon:
  bpf_object: ${repo_dir}/bpf/enforce.bpf.o
  control_socket: ${runtime_dir}/ctl.sock
  event_socket: ${runtime_dir}/events.sock
  launch_socket: ${runtime_dir}/launch.sock
  dsh_socket: ${runtime_dir}/dsh.sock
  ui_socket: ${runtime_dir}/ui.sock
  state_dir: ${runtime_dir}/state
rules:
  - file deny /etc/shadow
EOF

"${repo_dir}/target/release/censorguardd" launch --config "${runtime_dir}/base.yaml" \
  --duration 20 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 1200); do grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break; sleep 0.05; done
if ! grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
  echo "daemon did not become READY; daemon.log follows:" >&2
  cat "${runtime_dir}/daemon.log" >&2
  exit 1
fi

"${repo_dir}/target/release/censorguardctl" --config "${runtime_dir}/base.yaml" audit >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

# Verify launch already installed the baseline using a separate agent tree.
# Attaching this supervisor shell would also seed its daemon child into the domain.
sleep 0.1
if "${repo_dir}/target/release/censorguardctl" --config "${runtime_dir}/base.yaml" \
  spawn --domain startup -- /usr/bin/cat /etc/shadow >/dev/null 2>"${runtime_dir}/read.err"; then
  echo 'expected /etc/shadow read to be denied' >&2
  exit 1
fi
grep -q 'Operation not permitted' "${runtime_dir}/read.err"
for _ in $(seq 1 40); do
  grep -q 'DENY' "${runtime_dir}/audit.log" && break
  sleep 0.05
done
grep -q 'DENY' "${runtime_dir}/audit.log"
echo "Censorguard 0.5.0 startup smoke test passed"
