#!/usr/bin/env bash
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/lib-cli.sh"

# Failure checkpoint: report script/line and dump the daemon log tail.
on_err() {
    local rc=$?
    echo "[FAIL] $(basename "$0"):${1} rc=${rc}" >&2
    for d in "${runtime_dir:-}" "${lab_dir:-}"; do
        if [[ -n ${d} && -f ${d}/daemon.log ]]; then
            echo "--- ${d}/daemon.log (last 30 lines) ---" >&2
            tail -30 "${d}/daemon.log" >&2
        fi
    done
}
trap 'on_err "${LINENO}"' ERR
set -euo pipefail

# 断言依赖系统工具的英文输出 (id(1) strerror(3) 等会被 locale 本地化),
# 统一固定 C locale, 避免 zh_CN 环境下 grep 误判。
export LC_ALL=C

# Failure checkpoint: report script/line and dump the daemon log tail.
on_err() {
    local rc=$?
    echo "[FAIL] $(basename "$0"):${1} rc=${rc}" >&2
    for d in "${runtime_dir:-}" "${lab_dir:-}"; do
        if [[ -n ${d} && -f ${d}/daemon.log ]]; then
            echo "--- ${d}/daemon.log (last 30 lines) ---" >&2
            tail -30 "${d}/daemon.log" >&2
        fi
    done
}
trap 'on_err "${LINENO}"' ERR

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-runtime
daemon_pid=
audit_pid=
victim_pid=

cleanup() {
    for pid in "${victim_pid}" "${audit_pid}" "${daemon_pid}"; do
        if [[ -n ${pid} ]]; then
            kill "${pid}" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
printf 'secret\n' >"${runtime_dir}/protected.txt"
cat >"${runtime_dir}/policy.yaml" <<YAML
rules:
  - file deny ${runtime_dir}/protected.txt
groups:
  lab-policy:
    rules:
      - exec deny /usr/bin/id
      - exec deny /usr/bin/git push
domains:
  - name: lab-agent
    group: lab-policy
YAML
for file in daemon.log audit.log ctl.log status.log error.log output.log rc start; do
    : >"${runtime_dir}/${file}"
done
unlink "${runtime_dir}/start"

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 6 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    if grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
        break
    fi
    /usr/bin/sleep 0.05
done
if ! grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
    cat "${runtime_dir}/daemon.log" >&2
    exit 1
fi

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" audit >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

/bin/bash -c '
while [[ ! -e /tmp/censorguard-runtime/start ]]; do
    /usr/bin/sleep 0.05
done
/usr/bin/cat /tmp/censorguard-runtime/protected.txt \
    >/tmp/censorguard-runtime/output.log 2>/tmp/censorguard-runtime/error.log
printf "%s\n" "$?" >/tmp/censorguard-runtime/rc
' &
victim_pid=$!

"${repo_dir}/target/debug/censorguardctl" \
    --config "${runtime_dir}/config.yaml" \
    attach --pid "${victim_pid}" --domain lab-agent >"${runtime_dir}/ctl.log"

touch "${runtime_dir}/start"
wait "${victim_pid}"
victim_pid=

"${repo_dir}/target/debug/censorguardctl" \
    --config "${runtime_dir}/config.yaml" status >"${runtime_dir}/status.log"

rc=$(<"${runtime_dir}/rc")
if [[ ${rc} -eq 0 || -s "${runtime_dir}/output.log" ]]; then
    echo "daemon integration did not block protected file read" >&2
    exit 1
fi

wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=

if ! grep -q 'DENY' "${runtime_dir}/audit.log"; then
    echo "audit stream did not contain a DENY event" >&2
    cat "${runtime_dir}/audit.log" >&2
    exit 1
fi
if ! grep -q '"tracked":' "${runtime_dir}/status.log"; then
    echo "status response is missing tracked count" >&2
    exit 1
fi
grep -q '"event_kernel_dropped": 0' "${runtime_dir}/status.log"
grep -q '"event_reader_dropped": 0' "${runtime_dir}/status.log"
grep -q '"event_subscriber_dropped": 0' "${runtime_dir}/status.log"
grep -q '"event_subscribers": 1' "${runtime_dir}/status.log"

echo "daemon/ctl/audit integration passed: cat rc=${rc}"
grep 'DENY' "${runtime_dir}/audit.log" | tail -n 1
