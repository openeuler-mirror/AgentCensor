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
runtime_dir=/tmp/censorguard-bind
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
rules:
  - file deny /tmp/censorguard/protected.txt
groups:
  lab-policy:
    rules:
      - exec deny /usr/bin/id
      - exec deny /usr/bin/git push
  allow-policy:
    rules: []
domains:
  - name: lab-agent
    group: lab-policy
YAML
: >"${runtime_dir}/daemon.log"

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 7 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

set +e
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/before.log" 2>&1
before_rc=$?
set -e
[[ ${before_rc} -ne 0 ]]

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    bind --domain lab-agent --policy allow-policy --json >"${runtime_dir}/bind.log"

# Additive bind keeps the old deny until that group is explicitly removed.
set +e
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/both.log" 2>&1
both_rc=$?
set -e
[[ ${both_rc} -ne 0 ]]
"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    remove --domain lab-agent --policy lab-policy >"${runtime_dir}/remove.log"

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
    spawn --domain lab-agent -- /usr/bin/id >"${runtime_dir}/after.log" 2>&1

grep -q '"version": 2' "${runtime_dir}/bind.log"
# id 的输出随 locale 本地化 (zh_CN 为 "用户id=0(root)"), 统一按 "(root)" 判
grep -q '(root)' "${runtime_dir}/after.log"

wait "${daemon_pid}"
daemon_pid=
echo "domain bind integration passed: additive bind and explicit removal preserve independent denies"
