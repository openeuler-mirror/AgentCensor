#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-execveat
daemon_pid=
audit_pid=

cleanup() {
    for pid in "${audit_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

run_denied() {
    local label=$1
    shift
    set +e
    "${repo_dir}/target/debug/censorguardctl" \
        --socket "${runtime_dir}/ctl.sock" spawn --domain execveat-domain -- \
        "$@" >"${runtime_dir}/${label}.log" 2>&1
    local rc=$?
    set -e
    if [[ ${rc} -eq 0 ]]; then
        echo "${label} unexpectedly succeeded" >&2
        cat "${runtime_dir}/${label}.log" >&2
        return 1
    fi
}

run_allowed() {
    local label=$1
    local expected=$2
    shift 2
    "${repo_dir}/target/debug/censorguardctl" \
        --socket "${runtime_dir}/ctl.sock" spawn --domain execveat-domain -- \
        "$@" >"${runtime_dir}/${label}.log" 2>&1
    grep -qx "${expected}" "${runtime_dir}/${label}.log"
}

install -d -m 0700 "${runtime_dir}"
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/execveat_probe.c" \
    -o "${runtime_dir}/execveat-probe"
ln -sfn /usr/bin/id "${runtime_dir}/id-alias"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  execveat-policy:
    rules:
      - exec deny /usr/bin/id
      - exec deny /usr/bin/echo blocked
domains:
  - name: execveat-domain
    group: execveat-policy
YAML
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/audit.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 25 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

# ALLOW 事件现在由运行时开关控制（替代旧 allow_sample_rate）
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    set --audit-exec on >/dev/null

"${repo_dir}/target/debug/censorguard-audit" \
    --socket "${runtime_dir}/events.sock" --kind 2 \
    >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!
/usr/bin/sleep 0.1

for mode in absolute relative empty; do
    run_denied "command-${mode}" \
        "${runtime_dir}/execveat-probe" "${mode}" /usr/bin/id
    run_denied "argument-${mode}" \
        "${runtime_dir}/execveat-probe" "${mode}" /usr/bin/echo blocked extra
    run_allowed "allowed-${mode}" "allowed-${mode}" \
        "${runtime_dir}/execveat-probe" "${mode}" /usr/bin/echo "allowed-${mode}"
done

# 普通 execve 通过软链接引用同一 inode，也必须命中 /usr/bin/id 规则。
run_denied command-alias "${runtime_dir}/id-alias"

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=

[[ $(grep -c 'DENY.*execveat-domain' "${runtime_dir}/audit.log") -eq 7 ]]
[[ $(grep -c 'DENY.*blocked' "${runtime_dir}/audit.log") -eq 3 ]]
for mode in absolute relative empty; do
    grep -q "ALLOW.*execveat-domain.*allowed-${mode}" "${runtime_dir}/audit.log"
done
grep -q "DENY.*execveat-domain.*id-alias" "${runtime_dir}/audit.log"

echo "execveat integration passed: absolute/relative/AT_EMPTY_PATH command+argument denied, alias denied, safe args allowed"
