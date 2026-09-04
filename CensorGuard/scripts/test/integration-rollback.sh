#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-rollback
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
: >"${runtime_dir}/daemon.log"
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
    --duration 12 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

set +e
{
    printf 'rules:\n'
    for index in $(seq 1 257); do
        printf '  - exec deny /tmp/censorguard-command-%s\n' "${index}"
    done
} | "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    reload --group lab-policy --stdin >"${runtime_dir}/overflow.log" 2>&1
overflow_rc=$?
set -e
[[ ${overflow_rc} -ne 0 ]]
grep -q 'maximum is 256' "${runtime_dir}/overflow.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/failure-status.log"
grep -q '"reload_gen": 0' "${runtime_dir}/failure-status.log"
grep -q '"version": 1' "${runtime_dir}/failure-status.log"
grep -q '"active_bank": 0' "${runtime_dir}/failure-status.log"
grep -q '"last_reload_time_ms":' "${runtime_dir}/failure-status.log"
grep -q '"last_reload_error": ".*maximum is 256' "${runtime_dir}/failure-status.log"

printf 'rules: []\n' | \
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    reload --group lab-policy --stdin --dry-run >"${runtime_dir}/dry-run.log"
grep -q '"reload_gen": 0' "${runtime_dir}/dry-run.log"
grep -q '"version": 1' "${runtime_dir}/dry-run.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/status.log"
grep -q '"reload_gen": 0' "${runtime_dir}/status.log"

set +e
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/after-failure.log" 2>&1
deny_rc=$?
set -e
[[ ${deny_rc} -ne 0 ]]

printf 'rules: []\n' | \
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    reload --group lab-policy --stdin >"${runtime_dir}/commit.log"
grep -q '"reload_gen": 1' "${runtime_dir}/commit.log"
grep -q '"version": 2' "${runtime_dir}/commit.log"
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/after-commit.log" 2>&1
grep -q 'uid=0' "${runtime_dir}/after-commit.log"

wait "${daemon_pid}"
daemon_pid=
echo "rollback/dry-run integration passed: overflow_rc=${overflow_rc}, old bank stayed active"
