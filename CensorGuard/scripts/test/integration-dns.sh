#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-dns
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/connect_probe.c" \
    -o "${runtime_dir}/connect-probe"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  lab-policy:
    rules:
      - net deny 10.0.0.0/8:22
      - net deny localhost:18081
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
    --duration 7 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    policy-dump >"${runtime_dir}/policy.log"
grep -q '"domain": "localhost"' "${runtime_dir}/policy.log"
grep -q '"127.0.0.1"' "${runtime_dir}/policy.log"
if grep -q '"stale": true' "${runtime_dir}/policy.log"; then
    echo "localhost DNS resolution unexpectedly used stale cache" >&2
    exit 1
fi

set +e
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- "${runtime_dir}/connect-probe" \
    127.0.0.1 18081 >"${runtime_dir}/connect.log" 2>&1
connect_rc=$?
set -e
[[ ${connect_rc} -ne 0 ]]
grep -q 'Operation not permitted' "${runtime_dir}/connect.log"

wait "${daemon_pid}"
daemon_pid=
echo "DNS network integration passed: localhost -> 127.0.0.1:18081 denied"
