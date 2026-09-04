#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-network
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
clang -O2 -Wall -Wextra -Werror "${repo_dir}/tools/connect_probe.c" \
    -o "${runtime_dir}/connect-probe"

"${repo_dir}/target/debug/censorguardd" \
    --config "${repo_dir}/config/base.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 8 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- "${runtime_dir}/connect-probe" \
    >"${runtime_dir}/before.log" 2>&1
grep -q 'errno=111' "${runtime_dir}/before.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    reload --stdin >"${runtime_dir}/reload.log" <<'YAML'
rules:
  - file deny /tmp/censorguard/protected.txt
groups:
  lab-policy:
    rules:
      - exec deny /usr/bin/id
      - net deny 127.0.0.1:18080
  allow-policy: {}
domains:
  - name: lab-agent
    group: lab-policy
YAML

set +e
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- "${runtime_dir}/connect-probe" \
    >"${runtime_dir}/after.log" 2>&1
after_rc=$?
set -e

[[ ${after_rc} -ne 0 ]]
grep -q 'errno=1 Operation not permitted' "${runtime_dir}/after.log"

wait "${daemon_pid}"
daemon_pid=
echo "network reload integration passed: ECONNREFUSED -> EPERM"

