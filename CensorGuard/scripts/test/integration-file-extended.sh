#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-file-extended
protected_file=${runtime_dir}/protected.bin
protected_dir=${runtime_dir}/protected-dir
delete_dir=${runtime_dir}/delete-dir
daemon_pid=
audit_pid=
probe_pid=

cleanup() {
    for pid in "${probe_pid}" "${audit_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill -KILL "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

for tool in cc setfattr getfattr setfacl getfacl; do
    command -v "${tool}" >/dev/null
done

install -d -m 0700 "${runtime_dir}"
for path in \
    "${runtime_dir}/hardlink" \
    "${protected_dir}/symlink" \
    "${protected_dir}/fifo" \
    "${protected_file}"; do
    unlink "${path}" 2>/dev/null || true
done
rmdir "${protected_dir}/subdir" "${protected_dir}" "${delete_dir}" 2>/dev/null || true
install -d -m 0700 "${protected_dir}" "${delete_dir}"
truncate -s 4096 "${protected_file}"
chmod 0644 "${protected_file}"
setfattr -n user.existing -v before "${protected_file}"
protected_hash=$(sha256sum "${protected_file}" | awk '{ print $1 }')

cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/file_hook_probe.c" \
    -o "${runtime_dir}/file-hook-probe"

cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  guarded:
    rules:
      - file deny ${protected_file} [read,write,rename,attr]
      - file deny ${protected_dir} [write,delete,rename,attr]
      - file deny ${delete_dir} [delete]
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
    --duration 30 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

"${repo_dir}/target/debug/censorguard-audit" --socket "${runtime_dir}/events.sock" \
    --kind 1 >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!
for _ in $(seq 1 200); do
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        status >"${runtime_dir}/status.log"
    grep -q '"event_subscribers": 1' "${runtime_dir}/status.log" && break
    /usr/bin/sleep 0.01
done
grep -q '"event_subscribers": 1' "${runtime_dir}/status.log"

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
        cat "${runtime_dir}/${name}.log" >&2
        exit 1
    fi
    grep -q 'Operation not permitted' "${runtime_dir}/${name}.log"
}

run_preopened_denied() {
    local mode=$1
    "${runtime_dir}/file-hook-probe" "${mode}" "${protected_file}" \
        >"${runtime_dir}/${mode}.log" 2>&1 &
    probe_pid=$!
    for _ in $(seq 1 200); do
        grep -q '^State:[[:space:]]*T' "/proc/${probe_pid}/status" 2>/dev/null && break
        /usr/bin/sleep 0.01
    done
    grep -q '^State:[[:space:]]*T' "/proc/${probe_pid}/status"
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        attach --pid "${probe_pid}" --domain file-domain >"${runtime_dir}/${mode}-attach.log"
    kill -CONT "${probe_pid}"
    set +e
    wait "${probe_pid}"
    local result=$?
    set -e
    local finished_pid=${probe_pid}
    probe_pid=
    if [[ ${result} -ne 1 ]]; then
        echo "${mode} returned ${result}, expected EPERM result 1" >&2
        cat "${runtime_dir}/${mode}.log" >&2
        exit 1
    fi
    grep -q 'errno=1 Operation not permitted' "${runtime_dir}/${mode}.log"
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        untrack --pid "${finished_pid}" >/dev/null 2>&1 || true
}

run_denied link /usr/bin/ln "${protected_file}" "${runtime_dir}/hardlink"
run_denied symlink /usr/bin/ln -s /tmp/target "${protected_dir}/symlink"
run_denied mkdir /usr/bin/mkdir "${protected_dir}/subdir"
run_denied mknod /usr/bin/mknod "${protected_dir}/fifo" p
run_denied rmdir /usr/bin/rmdir "${delete_dir}"
run_denied chown /usr/bin/chown 65534:65534 "${protected_file}"
run_denied setxattr /usr/bin/setfattr -n user.blocked -v blocked "${protected_file}"
run_denied removexattr /usr/bin/setfattr -x user.existing "${protected_file}"
run_denied setacl /usr/bin/setfacl -m u:65534:r "${protected_file}"
run_preopened_denied read
run_preopened_denied write
run_preopened_denied ftruncate
run_preopened_denied mmap
run_preopened_denied mprotect

[[ ! -e ${runtime_dir}/hardlink ]]
[[ ! -e ${protected_dir}/symlink ]]
[[ ! -e ${protected_dir}/subdir ]]
[[ ! -e ${protected_dir}/fifo ]]
[[ -d ${delete_dir} ]]
[[ $(stat -c '%u:%g:%s' "${protected_file}") == 0:0:4096 ]]
[[ $(sha256sum "${protected_file}" | awk '{ print $1 }') == "${protected_hash}" ]]
[[ $(getfattr --absolute-names --only-values -n user.existing "${protected_file}") == before ]]
if getfattr --absolute-names --only-values -n user.blocked \
    "${protected_file}" >/dev/null 2>&1; then
    echo "blocked xattr was unexpectedly created" >&2
    exit 1
fi
if getfacl -cp "${protected_file}" | grep -q '^user:nobody:'; then
    echo "blocked ACL entry was unexpectedly created" >&2
    exit 1
fi

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=

for operation in \
    link symlink mkdir mknod rmdir chown setxattr removexattr setacl \
    fd_read fd_write ftruncate mmap_write mprotect; do
    if ! grep -Eq "^FILE +${operation} +[0-9]+ +DENY" "${runtime_dir}/audit.log"; then
        echo "missing DENY audit operation ${operation}" >&2
        cat "${runtime_dir}/audit.log" >&2
        exit 1
    fi
done

echo "extended file hook integration passed: 14 operations denied"
