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
runtime_dir=/tmp/censorguard-file-ops
protected_file=${runtime_dir}/protected.txt
daemon_pid=
audit_pid=

cleanup() {
    for pid in "${audit_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
printf 'original\n' >"${protected_file}"
chmod 0644 "${protected_file}"
cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  guarded:
    rules:
      - file deny ${protected_file} [write,delete,rename,attr]
domains:
  - name: file-domain
    group: guarded
YAML
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/audit.log"

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/config.yaml" \
    --duration 30 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!
echo "step: daemon starting (pid ${daemon_pid})"

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
if ! grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
    echo "daemon did not become READY; daemon.log follows:" >&2
    cat "${runtime_dir}/daemon.log" >&2
    exit 1
fi
echo "step: daemon READY"

"${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" audit \
    --kind 1 >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

run_denied() {
    local name=$1
    shift
    set +e
    "${repo_dir}/target/debug/censorguardctl" --config "${runtime_dir}/config.yaml" \
        spawn --domain file-domain -- "$@" >"${runtime_dir}/${name}.log" 2>&1
    local result=$?
    set -e
    if [[ ${result} -eq 0 ]]; then
        echo "${name} unexpectedly succeeded" >&2
        exit 1
    fi
    grep -q 'Operation not permitted' "${runtime_dir}/${name}.log"
    echo "step: ${name} denied as expected"
}

run_denied write /bin/bash -c "printf changed >>'${protected_file}'"
run_denied delete /usr/bin/rm -f "${protected_file}"
run_denied rename /usr/bin/mv "${protected_file}" "${runtime_dir}/moved.txt"
run_denied attr /usr/bin/chmod 0600 "${protected_file}"

[[ $(<"${protected_file}") == original ]]
[[ ! -e ${runtime_dir}/moved.txt ]]
[[ $(stat -c '%a' "${protected_file}") == 644 ]]
echo "step: content/mode intact after denied ops"

wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=
deny_count=$(grep -c 'DENY' "${runtime_dir}/audit.log" || true)
echo "step: audit DENY events = ${deny_count}"
if [[ ${deny_count} -lt 4 ]]; then
    echo "expected >= 4 DENY events; audit.log follows:" >&2
    cat "${runtime_dir}/audit.log" >&2
    exit 1
fi

echo "file operation integration passed: write/delete/rename/attr all denied"
