#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-exec-domains
daemon_pid=
audit_pid=

cleanup() {
    for pid in "${audit_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  arg-policy:
    rules:
      - exec deny /usr/bin/echo blocked
  deny-policy:
    rules:
      - exec deny /usr/bin/id
  allow-policy:
    rules: []
domains:
  - name: arg-domain
    group: arg-policy
  - name: deny-domain
    group: deny-policy
  - name: allow-domain
    group: allow-policy
YAML
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/audit.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 10 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

# ALLOW 事件现在由运行时开关控制（替代旧 allow_sample_rate）
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    set --audit-exec on >/dev/null

"${repo_dir}/target/debug/censorguard-audit" --socket "${runtime_dir}/events.sock" \
    --kind 2 >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain arg-domain -- /usr/bin/echo allowed >"${runtime_dir}/arg-allow.log" 2>&1
grep -q '^allowed$' "${runtime_dir}/arg-allow.log"

set +e
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain arg-domain -- /usr/bin/echo blocked extra \
    >"${runtime_dir}/arg-deny.log" 2>&1
arg_rc=$?
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain deny-domain -- /usr/bin/id >"${runtime_dir}/cmd-deny.log" 2>&1
deny_rc=$?
set -e
[[ ${arg_rc} -ne 0 ]]
[[ ${deny_rc} -ne 0 ]]

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain allow-domain -- /usr/bin/id >"${runtime_dir}/cmd-allow.log" 2>&1
grep -q 'uid=0' "${runtime_dir}/cmd-allow.log"

wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=
grep -q 'DENY.*arg-domain.*echo' "${runtime_dir}/audit.log"
grep -q 'DENY.*deny-domain.*id' "${runtime_dir}/audit.log"
grep -q 'ALLOW.*allow-domain.*id' "${runtime_dir}/audit.log"

echo "exec/domain integration passed: argument prefix and domain isolation enforced"
