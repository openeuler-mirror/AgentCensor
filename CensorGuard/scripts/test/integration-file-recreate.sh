#!/usr/bin/env bash

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
# Regression: file rules must survive delete+recreate (new inode).

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
#

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
# Phase A: rule with default mask (DENY_ALL). The file is recreated by the

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
# untracked test shell after the daemon has loaded the policy (inode changes).

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
# A tracked victim must still be denied read/write/delete/rename/chmod via the

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
# path-walk fallback (file_str dimension).

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
#

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
# Phase B: rule with [write] only (no delete bit). A tracked victim deletes the

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
# file (allowed) and recreates it with a write redirection — the original bug.

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
# The create must be denied at path_mknod/file_open via the string identity.

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

start_daemon() {
    local lab_dir=$1 domain=$2
    : >"${lab_dir}/daemon.log"
    echo "step: starting daemon for ${domain} (victim pid $3)"
    "${repo_dir}/target/debug/censorguardd" \
        --config "${lab_dir}/policy.yaml" \
        --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
        --ctl-sock "${lab_dir}/ctl.sock" \
        --event-sock "${lab_dir}/events.sock" \
        --track-pid "$3" \
        --domain "${domain}" \
        --hold-seconds 3 >"${lab_dir}/daemon.log" 2>&1 &
    daemon_pid=$!
    for _ in $(seq 1 1200); do
        if grep -q '^\[READY\]' "${lab_dir}/daemon.log"; then
            echo "step: daemon READY (${domain})"
            return 0
        fi
        /usr/bin/sleep 0.05
    done
    echo "daemon did not become READY; daemon.log follows:" >&2
    cat "${lab_dir}/daemon.log" >&2
    return 1
}

# ---------- Phase A: DENY_ALL + external recreate ----------
lab_a=/tmp/censorguard-recreate-a
rm -rf "${lab_a}"
install -d -m 0700 "${lab_a}"
printf 'secret\n' >"${lab_a}/protected.txt"
cat >"${lab_a}/policy.yaml" <<YAML
rules:
  - file deny ${lab_a}/protected.txt
domains:
  - name: lab-recreate-a
YAML
unlink "${lab_a}/start" 2>/dev/null || true

/bin/bash -c '
lab=/tmp/censorguard-recreate-a
while [[ ! -e "$lab/start" ]]; do /usr/bin/sleep 0.05; done
cat "$lab/protected.txt" >/dev/null 2>&1; echo $? >"$lab/rc_read"
echo x >>"$lab/protected.txt" 2>/dev/null; echo $? >"$lab/rc_write"
rm -f "$lab/protected.txt" 2>/dev/null; echo $? >"$lab/rc_rm"
mv "$lab/protected.txt" "$lab/moved.txt" 2>/dev/null; echo $? >"$lab/rc_mv"
chmod 600 "$lab/protected.txt" 2>/dev/null; echo $? >"$lab/rc_chmod"
' &
victim_a=$!

start_daemon "${lab_a}" lab-recreate-a "${victim_a}"

# Recreate outside the tracked tree: the inode changes after policy load.
rm -f "${lab_a}/protected.txt"
printf 'recreated\n' >"${lab_a}/protected.txt"
echo "step: phase A file recreated externally (new inode), releasing victim"

touch "${lab_a}/start"
wait "${victim_a}"
wait "${daemon_pid}"
echo "step: phase A victim done, checking rcs"

for op in read write rm mv chmod; do
    rc=$(<"${lab_a}/rc_${op}")
    if [[ ${rc} -eq 0 ]]; then
        echo "phase A: ${op} on recreated file unexpectedly succeeded" >&2
        exit 1
    fi
done
echo "phase A passed: recreated file still fully denied"

# ---------- Phase B: [write] mask + in-scope recreate (original bug) ----------
lab_b=/tmp/censorguard-recreate-b
rm -rf "${lab_b}"
install -d -m 0700 "${lab_b}"
printf 'secret\n' >"${lab_b}/target.txt"
cat >"${lab_b}/policy.yaml" <<YAML
rules:
  - file deny ${lab_b}/target.txt [write]
domains:
  - name: lab-recreate-b
YAML
unlink "${lab_b}/start" 2>/dev/null || true

/bin/bash -c '
lab=/tmp/censorguard-recreate-b
while [[ ! -e "$lab/start" ]]; do /usr/bin/sleep 0.05; done
# delete is allowed (mask has no delete bit)...
rm -f "$lab/target.txt" 2>/dev/null; echo $? >"$lab/rc_rm"
# ...but recreating it with a write must be denied even with a fresh inode
echo payload >"$lab/target.txt" 2>/dev/null; echo $? >"$lab/rc_write"
' &
victim_b=$!

start_daemon "${lab_b}" lab-recreate-b "${victim_b}"

touch "${lab_b}/start"
wait "${victim_b}"
wait "${daemon_pid}"
echo "step: phase B victim done, checking rcs"

rc_rm=$(<"${lab_b}/rc_rm")
rc_write=$(<"${lab_b}/rc_write")
if [[ ${rc_rm} -ne 0 ]]; then
    echo "phase B: delete should be allowed under [write] mask, got rc=${rc_rm}" >&2
    exit 1
fi
if [[ ${rc_write} -eq 0 ]]; then
    echo "phase B: write to recreated file unexpectedly succeeded (inode escape)" >&2
    exit 1
fi
if [[ -f "${lab_b}/target.txt" ]] && grep -q payload "${lab_b}/target.txt"; then
    echo "phase B: payload landed in recreated target" >&2
    exit 1
fi
echo "phase B passed: in-scope recreate+write denied"

# ---------- Phase C: path longer than the old 63-byte key limit ----------
lab_c=/tmp/censorguard-recreate-c
rm -rf "${lab_c}"
install -d -m 0700 "${lab_c}/subdir-with-a-fairly-long-name-0123456789"
long_file="${lab_c}/subdir-with-a-fairly-long-name-0123456789/target-with-a-long-name-abcdef.txt"
printf 'secret\n' >"${long_file}"
if [[ ${#long_file} -le 63 || ${#long_file} -gt 255 ]]; then
    echo "phase C path length ${#long_file} outside (63,255]" >&2
    exit 1
fi
cat >"${lab_c}/policy.yaml" <<YAML
rules:
  - file deny ${long_file}
domains:
  - name: lab-recreate-c
YAML
unlink "${lab_c}/start" 2>/dev/null || true

/bin/bash -c '
lab=/tmp/censorguard-recreate-c
f="$lab/subdir-with-a-fairly-long-name-0123456789/target-with-a-long-name-abcdef.txt"
while [[ ! -e "$lab/start" ]]; do /usr/bin/sleep 0.05; done
cat "$f" >/dev/null 2>&1; echo $? >"$lab/rc_read"
echo x >>"$f" 2>/dev/null; echo $? >"$lab/rc_write"
' &
victim_c=$!

start_daemon "${lab_c}" lab-recreate-c "${victim_c}"

rm -f "${long_file}"
printf 'recreated\n' >"${long_file}"
echo "step: phase C long-path (${#long_file}B) file recreated externally, releasing victim"

touch "${lab_c}/start"
wait "${victim_c}"
wait "${daemon_pid}"
echo "step: phase C victim done, checking rcs"

for op in read write; do
    rc=$(<"${lab_c}/rc_${op}")
    if [[ ${rc} -eq 0 ]]; then
        echo "phase C: ${op} on recreated long-path file unexpectedly succeeded" >&2
        exit 1
    fi
done
echo "phase C passed: long-path (>63B) recreated file still denied"

echo "file recreate integration passed"
