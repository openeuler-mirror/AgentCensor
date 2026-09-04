#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-self-protect
daemon_pid=
audit_pid=

cleanup() {
    [[ -n ${audit_pid} ]] && kill "${audit_pid}" 2>/dev/null || true
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
domains:
  - name: lab-agent
YAML
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/security_probe.c" \
    -o "${runtime_dir}/security-probe"
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

"${repo_dir}/target/debug/censorguard-audit" --socket "${runtime_dir}/events.sock" \
    --kind 4 >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

set +e
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    spawn --domain lab-agent -- /bin/bash -c "kill -TERM ${daemon_pid}" \
    >"${runtime_dir}/attack.log" 2>&1
attack_rc=$?
set -e

[[ ${attack_rc} -ne 0 ]]
kill -0 "${daemon_pid}"
grep -q 'Operation not permitted' "${runtime_dir}/attack.log"

for mode in ptrace traceme bpf; do
    arguments=("${mode}")
    [[ ${mode} == ptrace ]] && arguments+=("${daemon_pid}")
    set +e
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain lab-agent -- "${runtime_dir}/security-probe" "${arguments[@]}" \
        >"${runtime_dir}/${mode}.log" 2>&1
    result=$?
    set -e
    [[ ${result} -ne 0 ]]
    grep -q 'errno=1 Operation not permitted' "${runtime_dir}/${mode}.log"
done

wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=
grep -q 'DENY.*ptrace tgid=' "${runtime_dir}/audit.log"
grep -q 'DENY.*ptrace_traceme' "${runtime_dir}/audit.log"
grep -q 'DENY.*bpf cmd=' "${runtime_dir}/audit.log"
echo "self-protection integration passed: kill/ptrace/traceme/bpf denied"
