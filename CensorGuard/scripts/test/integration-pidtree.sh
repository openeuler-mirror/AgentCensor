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
runtime_dir=/tmp/censorguard-pidtree
daemon_pid=
fork_root_pid=
sleep_one=
sleep_two=

cleanup() {
    for pid in "${sleep_one}" "${sleep_two}" "${fork_root_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/fork_only_probe.c" \
    -o "${runtime_dir}/fork-only-probe"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  empty:
    rules: []
domains:
  - name: fork-domain
    group: empty
  - name: multi-domain
    group: empty
YAML
unlink "${runtime_dir}/go" 2>/dev/null || true
unlink "${runtime_dir}/child.pid" 2>/dev/null || true
: >"${runtime_dir}/daemon.log"

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 13 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

/usr/bin/sleep 14 & sleep_one=$!
/usr/bin/sleep 14 & sleep_two=$!
for pid in "${sleep_one}" "${sleep_two}"; do
    "${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
        attach --pid "${pid}" --domain multi-domain >"${runtime_dir}/attach-${pid}.log"
done
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status >"${runtime_dir}/multi-two.log"
grep -q '"name": "multi-domain"' "${runtime_dir}/multi-two.log"
grep -q '"roots": 2' "${runtime_dir}/multi-two.log"

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    unattach --pid "${sleep_one}" >/dev/null
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status >"${runtime_dir}/multi-one.log"
grep -q '"roots": 1' "${runtime_dir}/multi-one.log"
if grep -A8 '"name": "multi-domain"' "${runtime_dir}/multi-one.log" | \
    grep -q '"draining": true'; then
    echo "domain entered draining while one root remained" >&2
    exit 1
fi
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    unattach --pid "${sleep_two}" >/dev/null

"${runtime_dir}/fork-only-probe" "${runtime_dir}/go" \
    "${runtime_dir}/child.pid" 7 &
fork_root_pid=$!
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    attach --pid "${fork_root_pid}" --domain fork-domain >"${runtime_dir}/fork-attach.log"
touch "${runtime_dir}/go"
for _ in $(seq 1 100); do
    [[ -s ${runtime_dir}/child.pid ]] && break
    /usr/bin/sleep 0.01
done
fork_child_pid=$(<"${runtime_dir}/child.pid")
kill -0 "${fork_child_pid}"

# The deadline is intentionally below the 5-second userspace reconcile interval. Passing proves
# sched_process_fork inserted the non-exec child directly into tracked_pids.
immediate=false
for _ in $(seq 1 50); do
    "${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
        status >"${runtime_dir}/fork-status.log"
    if grep -q '"tracked": 2' "${runtime_dir}/fork-status.log"; then
        immediate=true
        break
    fi
    /usr/bin/sleep 0.01
done
if [[ ${immediate} != true ]]; then
    echo "fork child was not immediately tracked" >&2
    cat "${runtime_dir}/fork-status.log" >&2
    exit 1
fi
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status >"${runtime_dir}/fork-tree.log"
grep -q "\"pid\": ${fork_child_pid}" "${runtime_dir}/fork-tree.log"

wait "${fork_root_pid}"
fork_root_pid=
kill "${sleep_one}" "${sleep_two}" 2>/dev/null || true
wait "${sleep_one}" 2>/dev/null || true
wait "${sleep_two}" 2>/dev/null || true
sleep_one=
sleep_two=

for _ in $(seq 1 240); do
    "${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
        status >"${runtime_dir}/final.log"
    if grep -q '"tracked": 0' "${runtime_dir}/final.log" && \
        ! grep -q '"name": "fork-domain"' "${runtime_dir}/final.log" && \
        ! grep -q '"name": "multi-domain"' "${runtime_dir}/final.log"; then
        break
    fi
    /usr/bin/sleep 0.05
done
grep -q '"tracked": 0' "${runtime_dir}/final.log"
if grep -q '"name": "fork-domain"\|"name": "multi-domain"' "${runtime_dir}/final.log"; then
    echo "PID-tree domains were not finalized" >&2
    exit 1
fi

wait "${daemon_pid}"
daemon_pid=
echo "PID-tree integration passed: immediate fork inheritance and multi-root lifecycle"
