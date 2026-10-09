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

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 18 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    spawn --keep --domain lab-agent -- /bin/bash -c \
    'setsid /usr/bin/sleep 10 </dev/null >/dev/null 2>&1 & echo $! >/tmp/censorguard-draining/child.pid'
child_pid=$(<"${runtime_dir}/child.pid")
kill -0 "${child_pid}"

for _ in $(seq 1 160); do
    "${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
        status >"${runtime_dir}/draining.log"
    if grep -q '"draining": true' "${runtime_dir}/draining.log"; then
        break
    fi
    /usr/bin/sleep 0.05
done
grep -q '"roots": 0' "${runtime_dir}/draining.log"
grep -q '"draining": true' "${runtime_dir}/draining.log"
grep -q '"tracked": 1' "${runtime_dir}/draining.log"
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status >"${runtime_dir}/tree.log"
grep -q "\"pid\": ${child_pid}" "${runtime_dir}/tree.log"

for _ in $(seq 1 320); do
    "${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
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
