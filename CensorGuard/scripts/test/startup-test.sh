#!/usr/bin/env bash
set -Eeuo pipefail

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
for _ in $(seq 1 200); do grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break; sleep 0.05; done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

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
