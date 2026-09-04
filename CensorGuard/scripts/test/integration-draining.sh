#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-draining
daemon_pid=
child_pid=

cleanup() {
    [[ -n ${child_pid} ]] && kill "${child_pid}" 2>/dev/null || true
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
: >"${runtime_dir}/daemon.log"
unlink "${runtime_dir}/child.pid" 2>/dev/null || true

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 18 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --keep --domain lab-agent -- /bin/bash -c \
    'setsid /usr/bin/sleep 10 </dev/null >/dev/null 2>&1 & echo $! >/tmp/censorguard-draining/child.pid'
child_pid=$(<"${runtime_dir}/child.pid")
kill -0 "${child_pid}"

for _ in $(seq 1 160); do
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        status >"${runtime_dir}/draining.log"
    if grep -q '"draining": true' "${runtime_dir}/draining.log"; then
        break
    fi
    /usr/bin/sleep 0.05
done
grep -q '"roots": 0' "${runtime_dir}/draining.log"
grep -q '"draining": true' "${runtime_dir}/draining.log"
grep -q '"tracked": 1' "${runtime_dir}/draining.log"
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    tree >"${runtime_dir}/tree.log"
grep -q "\"pid\": ${child_pid}" "${runtime_dir}/tree.log"

for _ in $(seq 1 320); do
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        status >"${runtime_dir}/final.log"
    if grep -q '"tracked": 0' "${runtime_dir}/final.log" && \
        ! grep -q '"name": "lab-agent"' "${runtime_dir}/final.log"; then
        break
    fi
    /usr/bin/sleep 0.05
done
grep -q '"tracked": 0' "${runtime_dir}/final.log"
if grep -q '"name": "lab-agent"' "${runtime_dir}/final.log"; then
    echo "draining domain was not finalized after its last descendant exited" >&2
    exit 1
fi
child_pid=

wait "${daemon_pid}"
daemon_pid=
echo "PID-tree draining integration passed: orphan descendant stayed tracked until exit"
