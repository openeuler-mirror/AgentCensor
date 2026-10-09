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
runtime_dir=/tmp/censorguard-auth
daemon_pid=
nobody_pid=
root_pid=

cleanup() {
    for pid in "${nobody_pid}" "${root_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

command -v setpriv >/dev/null
install -d -m 0755 "${runtime_dir}"
install -m 0755 "${repo_dir}/target/debug/censorguardctl" \
    "${runtime_dir}/censorguardctl"
cat >"${runtime_dir}/policy.yaml" <<'YAML'
groups:
  empty:
    rules: []
domains:
  - name: auth-domain
    group: empty
YAML
: >"${runtime_dir}/daemon.log"

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}" "${runtime_dir}/state" nobody
setpriv --no-new-privs \
    --bounding-set=-all,+chown,+dac_read_search,+sys_admin,+sys_resource,+perfmon,+bpf \
    "${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 8 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"
[[ $(stat -c '%a' "${runtime_dir}/ctl.sock") == 660 ]]
[[ $(stat -c '%g' "${runtime_dir}/ctl.sock") == 65534 ]]
[[ $(stat -c '%a' "${runtime_dir}/events.sock") == 660 ]]
[[ $(stat -c '%g' "${runtime_dir}/events.sock") == 65534 ]]

setpriv --reuid=65534 --regid=65534 --clear-groups /usr/bin/sleep 12 &
nobody_pid=$!
/usr/bin/sleep 12 &
root_pid=$!

# The background PID initially belongs to the short-lived setpriv process. Wait until setpriv has
# applied the credentials and exec'd sleep before asking the daemon to authorize the PID.
for _ in $(seq 1 1200); do
    nobody_uid=$(sed -n 's/^Uid:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
        "/proc/${nobody_pid}/status" 2>/dev/null || true)
    [[ ${nobody_uid} == 65534 ]] && break
    /usr/bin/sleep 0.01
done
[[ ${nobody_uid:-} == 65534 ]]

setpriv --reuid=65534 --regid=65534 --clear-groups \
    "${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    attach --pid "${nobody_pid}" --domain auth-domain >"${runtime_dir}/own-attach.log"
grep -q '"ok": true' "${runtime_dir}/own-attach.log"

set +e
setpriv --reuid=65534 --regid=65534 --clear-groups \
    "${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    attach --pid "${root_pid}" --domain auth-domain >"${runtime_dir}/foreign-attach.log" 2>&1
foreign_attach_rc=$?
set -e
[[ ${foreign_attach_rc} -ne 0 ]]
grep -q 'cannot track pid' "${runtime_dir}/foreign-attach.log"

"${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    attach --pid "${root_pid}" --domain auth-domain >"${runtime_dir}/root-attach.log"
set +e
setpriv --reuid=65534 --regid=65534 --clear-groups \
    "${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    unattach --pid "${root_pid}" >"${runtime_dir}/foreign-untrack.log" 2>&1
foreign_untrack_rc=$?
printf 'rules: []\n' | \
    setpriv --reuid=65534 --regid=65534 --clear-groups \
    "${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    policy apply --policy empty --stdin --dry-run >"${runtime_dir}/reload.log" 2>&1
reload_rc=$?
setpriv --reuid=65534 --regid=65534 --clear-groups \
    "${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    bind --domain auth-domain --policy empty >"${runtime_dir}/bind.log" 2>&1
bind_rc=$?
set -e

[[ ${foreign_untrack_rc} -ne 0 ]]
[[ ${reload_rc} -ne 0 ]]
[[ ${bind_rc} -ne 0 ]]
grep -q 'cannot untrack root pid' "${runtime_dir}/foreign-untrack.log"
grep -q 'requires root' "${runtime_dir}/reload.log"
grep -q 'requires root' "${runtime_dir}/bind.log"

setpriv --reuid=65534 --regid=65534 --clear-groups \
    "${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    unattach --pid "${nobody_pid}" >"${runtime_dir}/own-untrack.log"
"${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    unattach --pid "${root_pid}" >"${runtime_dir}/root-untrack.log"
"${runtime_dir}/censorguardctl" --config "${runtime_dir}/config.yaml" \
    status >"${runtime_dir}/status.log"
grep -q '"tracked": 0' "${runtime_dir}/status.log"

wait "${daemon_pid}"
daemon_pid=
echo "socket-group/SO_PEERCRED integration passed: group access allowed without privilege bypass"
