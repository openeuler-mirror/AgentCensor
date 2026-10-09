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

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/config.yaml" \
    --duration 6 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

set +e
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    spawn --domain launcher-test -- /usr/bin/id >"${runtime_dir}/denied.log" 2>&1
denied_rc=$?
set -e
if [[ ${denied_rc} -eq 0 ]]; then
    echo "target first exec unexpectedly escaped policy" >&2
    cat "${runtime_dir}/denied.log" >&2
    exit 1
fi

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    spawn --domain launcher-test -- /usr/bin/true

marker="${runtime_dir}/must-not-exist"
set +e
cp "${runtime_dir}/config.yaml" "${runtime_dir}/missing-config.yaml"
sed -i "s#${runtime_dir}/ctl.sock#${runtime_dir}/missing.sock#" "${runtime_dir}/missing-config.yaml"
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/missing-config.yaml" \
    spawn --domain unavailable-daemon -- /usr/bin/touch "${marker}" >"${runtime_dir}/fail-closed.log" 2>&1
unavailable_rc=$?
set -e
[[ ${unavailable_rc} -ne 0 ]]
[[ ! -e ${marker} ]]

wait "${daemon_pid}"
daemon_pid=
echo "launcher race integration passed: first exec denied; unavailable daemon executed no target"
