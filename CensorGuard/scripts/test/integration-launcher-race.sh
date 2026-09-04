#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-launcher-race
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
rules:
  - exec deny /usr/bin/id
YAML
: >"${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --launch-sock "${runtime_dir}/launch.sock" \
    --duration 6 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

set +e
"${repo_dir}/target/debug/censorguard-exec" \
    --socket "${runtime_dir}/launch.sock" \
    --scope launcher-test \
    -- /usr/bin/id >"${runtime_dir}/denied.log" 2>&1
denied_rc=$?
set -e
if [[ ${denied_rc} -eq 0 ]]; then
    echo "target first exec unexpectedly escaped policy" >&2
    cat "${runtime_dir}/denied.log" >&2
    exit 1
fi

"${repo_dir}/target/debug/censorguard-exec" \
    --socket "${runtime_dir}/launch.sock" \
    --scope launcher-test \
    -- /usr/bin/true

marker="${runtime_dir}/must-not-exist"
set +e
"${repo_dir}/target/debug/censorguard-exec" \
    --socket "${runtime_dir}/missing.sock" \
    --scope unavailable-daemon \
    -- /usr/bin/touch "${marker}" >"${runtime_dir}/fail-closed.log" 2>&1
unavailable_rc=$?
set -e
[[ ${unavailable_rc} -ne 0 ]]
[[ ! -e ${marker} ]]

wait "${daemon_pid}"
daemon_pid=
echo "launcher race integration passed: first exec denied; unavailable daemon executed no target"
