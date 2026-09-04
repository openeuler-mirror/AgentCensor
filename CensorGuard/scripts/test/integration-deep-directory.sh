#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-deep-directory
protected_root=${runtime_dir}/protected
allowed_root=${runtime_dir}/allowed
extreme_root=${runtime_dir}/extreme
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill -KILL "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

case ${runtime_dir} in
    /tmp/censorguard-deep-directory) rm -rf -- "${runtime_dir}" ;;
    *) echo "refusing to remove unexpected test path: ${runtime_dir}" >&2; exit 1 ;;
esac
install -d -m 0700 "${protected_root}" "${allowed_root}" "${extreme_root}"

make_deep_file() {
    local root=$1
    local depth=$2
    local path=${root}
    local index
    for index in $(seq 1 "${depth}"); do
        path=${path}/d${index}
    done
    install -d -m 0700 "${path}"
    printf 'original-content\n' >"${path}/payload.txt"
    printf '%s\n' "${path}/payload.txt"
}

# 16 levels proves the old eight-level limit is gone. 40 levels exceeds the new 32-step budget
# even after accounting for an early filesystem root and therefore exercises fail-closed.
protected_file=$(make_deep_file "${protected_root}" 16)
allowed_file=$(make_deep_file "${allowed_root}" 16)
extreme_file=$(make_deep_file "${extreme_root}" 40)
protected_hash=$(sha256sum "${protected_file}" | awk '{ print $1 }')

cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  guarded:
    rules:
      - file deny ${protected_root} [read,write,delete,rename,attr]
domains:
  - name: deep-domain
    group: guarded
YAML
: >"${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --duration 30 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\] 29 required hooks attached' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\] 29 required hooks attached' "${runtime_dir}/daemon.log"

run_denied() {
    local name=$1
    shift
    set +e
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain deep-domain -- "$@" >"${runtime_dir}/${name}.log" 2>&1
    local result=$?
    set -e
    if [[ ${result} -eq 0 ]]; then
        echo "${name} unexpectedly succeeded" >&2
        cat "${runtime_dir}/${name}.log" >&2
        exit 1
    fi
    grep -q 'Operation not permitted' "${runtime_dir}/${name}.log"
}

run_allowed() {
    local name=$1
    shift
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain deep-domain -- "$@" >"${runtime_dir}/${name}.log" 2>&1
}

run_denied protected-read /usr/bin/cat "${protected_file}"
run_denied protected-write /usr/bin/bash -c "printf changed >>'${protected_file}'"
run_denied protected-delete /usr/bin/rm "${protected_file}"
run_allowed allowed-read /usr/bin/cat "${allowed_file}"
run_denied extreme-depth /usr/bin/cat "${extreme_file}"

grep -q '^original-content$' "${runtime_dir}/allowed-read.log"
[[ -f ${protected_file} ]]
[[ $(sha256sum "${protected_file}" | awk '{ print $1 }') == "${protected_hash}" ]]
[[ -f ${extreme_file} ]]

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=

echo "deep directory integration passed: depth 16 enforced, normal depth 16 allowed, depth 40 failed closed"
