#!/usr/bin/env bash
set -euo pipefail

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

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 9 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguard-audit" --socket "${runtime_dir}/events.sock" \
    --kind 1 >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!

run_denied() {
    local name=$1
    shift
    set +e
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain file-domain -- "$@" >"${runtime_dir}/${name}.log" 2>&1
    local result=$?
    set -e
    if [[ ${result} -eq 0 ]]; then
        echo "${name} unexpectedly succeeded" >&2
        exit 1
    fi
    grep -q 'Operation not permitted' "${runtime_dir}/${name}.log"
}

run_denied write /bin/bash -c "printf changed >>'${protected_file}'"
run_denied delete /usr/bin/rm -f "${protected_file}"
run_denied rename /usr/bin/mv "${protected_file}" "${runtime_dir}/moved.txt"
run_denied attr /usr/bin/chmod 0600 "${protected_file}"

[[ $(<"${protected_file}") == original ]]
[[ ! -e ${runtime_dir}/moved.txt ]]
[[ $(stat -c '%a' "${protected_file}") == 644 ]]

wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=
deny_count=$(grep -c 'DENY' "${runtime_dir}/audit.log")
[[ ${deny_count} -ge 4 ]]

echo "file operation integration passed: write/delete/rename/attr all denied"
