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
lab_dir=/tmp/censorguard

install -d -m 0700 "${lab_dir}"
printf 'secret\n' >"${lab_dir}/protected.txt"
cat >"${lab_dir}/policy.yaml" <<YAML
rules:
  - file deny ${lab_dir}/protected.txt
domains:
  - name: lab-agent
YAML
: >"${lab_dir}/daemon.log"
: >"${lab_dir}/error.log"
: >"${lab_dir}/output.log"
: >"${lab_dir}/start"
unlink "${lab_dir}/start"

/bin/bash -c '
while [[ ! -e /tmp/censorguard/start ]]; do
    /usr/bin/sleep 0.05
done
/usr/bin/cat /tmp/censorguard/protected.txt \
    >/tmp/censorguard/output.log 2>/tmp/censorguard/error.log
printf "%s\n" "$?" >/tmp/censorguard/rc
' &
victim_pid=$!

write_test_config "${lab_dir}/config.yaml" "${lab_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${lab_dir}"
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${lab_dir}/config.yaml" \
    --duration 3 >"${lab_dir}/daemon.log" 2>&1 &
daemon_pid=$!

ready=false
for _ in $(seq 1 1200); do
    if grep -q '^\[READY\]' "${lab_dir}/daemon.log"; then
        ready=true
        break
    fi
    /usr/bin/sleep 0.05
done
if [[ ${ready} != true ]]; then
    cat "${lab_dir}/daemon.log" >&2
    kill "${victim_pid}" "${daemon_pid}" 2>/dev/null || true
    exit 1
fi

"${repo_dir}/target/debug/censorguardctl" --config "${lab_dir}/config.yaml" attach --pid "${victim_pid}" --domain lab-agent

touch "${lab_dir}/start"
wait "${victim_pid}"
wait "${daemon_pid}"

rc=$(<"${lab_dir}/rc")
if [[ ${rc} -eq 0 ]]; then
    echo "expected protected file read to fail, but cat succeeded" >&2
    exit 1
fi
if [[ -s "${lab_dir}/output.log" ]]; then
    echo "protected contents escaped to output" >&2
    exit 1
fi

echo "file deny integration passed: cat rc=${rc}"
cat "${lab_dir}/error.log"

