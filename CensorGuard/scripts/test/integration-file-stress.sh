#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-file-stress
protected_root=${runtime_dir}/protected
allowed_root=${runtime_dir}/allowed
threads=16
iterations=5000
daemon_pid=
probe_pid=

cleanup() {
    [[ -n ${probe_pid} ]] && kill -KILL "${probe_pid}" 2>/dev/null || true
    [[ -n ${daemon_pid} ]] && kill -KILL "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

case ${runtime_dir} in
    /tmp/censorguard-file-stress) rm -rf -- "${runtime_dir}" ;;
    *) echo "refusing to remove unexpected test path: ${runtime_dir}" >&2; exit 1 ;;
esac

make_deep_file() {
    local root=$1
    local path=${root}
    local index
    for index in $(seq 1 12); do
        path=${path}/d${index}
    done
    install -d -m 0700 "${path}"
    truncate -s 4096 "${path}/payload.bin"
    printf '%s\n' "${path}/payload.bin"
}

protected_file=$(make_deep_file "${protected_root}")
allowed_file=$(make_deep_file "${allowed_root}")
protected_hash=$(sha256sum "${protected_file}" | awk '{ print $1 }')

cc -std=c11 -O2 -Wall -Wextra -Werror -pthread \
    "${repo_dir}/tools/file_stress_probe.c" -o "${runtime_dir}/file-stress-probe"

cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  guarded:
    rules:
      - file deny ${protected_root} [read,write]
domains:
  - name: stress-domain
    group: guarded
YAML
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/probe.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 90 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 200); do
    grep -q '^\[READY\] 29 required hooks attached' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\] 29 required hooks attached' "${runtime_dir}/daemon.log"

"${runtime_dir}/file-stress-probe" "${protected_file}" "${allowed_file}" \
    "${threads}" "${iterations}" >"${runtime_dir}/probe.log" 2>&1 &
probe_pid=$!
for _ in $(seq 1 200); do
    grep -q '^State:[[:space:]]*T' "/proc/${probe_pid}/status" 2>/dev/null && break
    /usr/bin/sleep 0.01
done
grep -q '^State:[[:space:]]*T' "/proc/${probe_pid}/status"

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    attach --pid "${probe_pid}" --domain stress-domain >"${runtime_dir}/attach.log"
kill -CONT "${probe_pid}"

responsive=0
for _ in $(seq 1 500); do
    state=$(sed -n 's/^State:[[:space:]]*\([A-Z]\).*/\1/p' \
        "/proc/${probe_pid}/status" 2>/dev/null || true)
    [[ -z ${state} || ${state} == Z ]] && break
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        status >"${runtime_dir}/status-live.log"
    responsive=$((responsive + 1))
    /usr/bin/sleep 0.02
done

set +e
wait "${probe_pid}"
probe_rc=$?
set -e
probe_pid=
if [[ ${probe_rc} -ne 0 ]]; then
    echo "file stress probe failed with ${probe_rc}" >&2
    cat "${runtime_dir}/probe.log" >&2
    exit 1
fi

expected_denied=$((threads * iterations * 4))
expected_allowed=$((threads * iterations * 2))
grep -q "denied_ok=${expected_denied} allowed_ok=${expected_allowed} errors=0" \
    "${runtime_dir}/probe.log"
[[ ${responsive} -gt 0 ]]
[[ $(sha256sum "${protected_file}" | awk '{ print $1 }') == "${protected_hash}" ]]

for _ in $(seq 1 200); do
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        status >"${runtime_dir}/status-final.log"
    grep -q '"tracked": 0' "${runtime_dir}/status-final.log" && break
    /usr/bin/sleep 0.01
done
grep -q '"tracked": 0' "${runtime_dir}/status-final.log"

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=

printf 'concurrent file stress passed: control_responses=%d %s\n' \
    "${responsive}" "$(cat "${runtime_dir}/probe.log")"
