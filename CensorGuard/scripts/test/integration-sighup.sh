#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-sighup
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
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
cat >"${runtime_dir}/policy.relaxed.yaml" <<'YAML'
groups:
  lab-policy:
    rules: []
domains:
  - name: lab-agent
    group: lab-policy
YAML
: >"${runtime_dir}/daemon.log"

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

set +e
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/before.log" 2>&1
before_rc=$?
set -e
[[ ${before_rc} -ne 0 ]]

cp "${runtime_dir}/policy.relaxed.yaml" "${runtime_dir}/policy.yaml"
kill -HUP "${daemon_pid}"

for _ in $(seq 1 200); do
    grep -q 'SIGHUP reload complete: generation=1 version=2' \
        "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q 'SIGHUP reload complete: generation=1 version=2' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/status.log"
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/after.log" 2>&1

grep -q '"reload_gen": 1' "${runtime_dir}/status.log"
grep -q '"version": 2' "${runtime_dir}/status.log"
grep -q 'uid=0' "${runtime_dir}/after.log"

wait "${daemon_pid}"
daemon_pid=
echo "SIGHUP reload integration passed: before_rc=${before_rc}, version=2"
