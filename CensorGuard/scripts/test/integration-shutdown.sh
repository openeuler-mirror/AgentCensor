#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-shutdown
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
: >"${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${repo_dir}/config/base.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"
[[ -S ${runtime_dir}/ctl.sock ]]
[[ -S ${runtime_dir}/events.sock ]]

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=
grep -q 'shutdown requested by signal' "${runtime_dir}/daemon.log"
grep -q 'censorguardd shutdown complete' "${runtime_dir}/daemon.log"
[[ ! -e ${runtime_dir}/ctl.sock ]]
[[ ! -e ${runtime_dir}/events.sock ]]

echo "graceful shutdown integration passed: SIGTERM exit=0 and sockets removed"
