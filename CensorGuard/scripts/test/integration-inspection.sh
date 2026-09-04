#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-inspection
daemon_pid=
root_pid=

cleanup() {
    [[ -n ${root_pid} ]] && kill "${root_pid}" 2>/dev/null || true
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
rules:
  - file deny /tmp/censorguard/protected.txt
groups:
  lab-policy:
    rules:
      - exec deny /usr/bin/id
      - exec deny /usr/bin/git push
domains:
  - name: lab-agent
    group: lab-policy
YAML
: >"${runtime_dir}/daemon.log"
unlink "${runtime_dir}/process.ready" 2>/dev/null || true

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
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

/bin/bash -c '
/usr/bin/sleep 7 &
echo ready >/tmp/censorguard-inspection/process.ready
wait
' &
root_pid=$!
for _ in $(seq 1 100); do
    [[ -e ${runtime_dir}/process.ready ]] && break
    /usr/bin/sleep 0.02
done

child_pid=$(<"/proc/${root_pid}/task/${root_pid}/children")
child_pid=${child_pid%% *}
[[ -n ${child_pid} ]]

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    attach --pid "${root_pid}" --domain lab-agent --seed >"${runtime_dir}/attach.log"
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    tree >"${runtime_dir}/tree.log"
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    policy-dump >"${runtime_dir}/policy.log"

grep -q '"domain": "lab-agent"' "${runtime_dir}/tree.log"
grep -q "\"pid\": ${root_pid}" "${runtime_dir}/tree.log"
grep -q "\"pid\": ${child_pid}" "${runtime_dir}/tree.log"
grep -q '"root": true' "${runtime_dir}/tree.log"
grep -q '"name": "__base__"' "${runtime_dir}/policy.log"
grep -q '"name": "lab-policy"' "${runtime_dir}/policy.log"
grep -q '"file deny /tmp/censorguard/protected.txt"' "${runtime_dir}/policy.log"
grep -q '"exec deny /usr/bin/id"' "${runtime_dir}/policy.log"

wait "${root_pid}"
root_pid=
wait "${daemon_pid}"
daemon_pid=
echo "tree/policy-dump integration passed: root and child are visible"
