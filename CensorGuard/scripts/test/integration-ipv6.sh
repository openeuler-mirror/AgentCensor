#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-ipv6
hostname=dynamic6.Censorguard.invalid
dns_port=18086
daemon_pid=
audit_pid=

cleanup() {
    for pid in "${audit_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill -KILL "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

case ${runtime_dir} in
    /tmp/censorguard-ipv6) rm -rf -- "${runtime_dir}" ;;
    *) echo "refusing to remove unexpected test path: ${runtime_dir}" >&2; exit 1 ;;
esac
install -d -m 0700 "${runtime_dir}"
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/connect_probe.c" \
    -o "${runtime_dir}/connect-probe"
printf 'hosts: files\n' >"${runtime_dir}/nsswitch.conf"

write_hosts() {
    local address=${1:-}
    printf '127.0.0.1 localhost\n::1 localhost\n' >"${runtime_dir}/hosts"
    if [[ -n ${address} ]]; then
        printf '%s %s\n' "${address}" "${hostname}" >>"${runtime_dir}/hosts"
    fi
}
write_hosts ::2

cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  ipv6-policy:
    rules:
      - net deny ::1/128
      - net allow [::1]:18084
      - net deny 2001:db8::/32
      - net deny ${hostname}:${dns_port}
domains:
  - name: ipv6-domain
    group: ipv6-policy
YAML
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/audit.log"

unshare --mount --propagation private /bin/bash -c '
    set -e
    mount --bind "$1" /etc/hosts
    mount --bind "$2" /etc/nsswitch.conf
    shift 2
    exec "$@"
' Censorguard-ipv6 \
    "${runtime_dir}/hosts" "${runtime_dir}/nsswitch.conf" \
    "${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --launch-sock "${runtime_dir}/launch.sock" \
    --dns-refresh-seconds 1 \
    --duration 40 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
if ! grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
    cat "${runtime_dir}/daemon.log" >&2
    exit 1
fi

# ALLOW auditing is a runtime switch now: enable global network allow auditing.
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    set --audit-net on >"${runtime_dir}/set-audit.log"

"${repo_dir}/target/debug/censorguard-audit" \
    --socket "${runtime_dir}/events.sock" --kind 3 \
    >"${runtime_dir}/audit.log" 2>&1 &
audit_pid=$!
/usr/bin/sleep 0.1

status_value() {
    local field=$1
    local file=$2
    sed -n "s/.*\"${field}\": \([0-9][0-9]*\).*/\1/p" "${file}"
}

probe_denied() {
    local address=$1
    local port=$2
    local name=$3
    set +e
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain ipv6-domain -- "${runtime_dir}/connect-probe" \
        "${address}" "${port}" >"${runtime_dir}/${name}.log" 2>&1
    local result=$?
    set -e
    [[ ${result} -ne 0 ]]
    grep -q 'Operation not permitted' "${runtime_dir}/${name}.log"
}

probe_allowed() {
    local address=$1
    local port=$2
    local name=$3
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain ipv6-domain -- "${runtime_dir}/connect-probe" \
        "${address}" "${port}" >"${runtime_dir}/${name}.log" 2>&1
    if grep -q 'Operation not permitted' "${runtime_dir}/${name}.log"; then
        cat "${runtime_dir}/${name}.log" >&2
        return 1
    fi
}

wait_dns_state() {
    local address=$1
    local version=$2
    local bank=$3
    local stale=$4
    local prefix=$5
    for _ in $(seq 1 120); do
        "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
            policy-dump >"${runtime_dir}/${prefix}-policy.log"
        "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
            status >"${runtime_dir}/${prefix}-status.log"
        if grep -q "\"${address}\"" "${runtime_dir}/${prefix}-policy.log" && \
            [[ $(status_value version "${runtime_dir}/${prefix}-status.log") == "${version}" ]] && \
            [[ $(status_value active_bank "${runtime_dir}/${prefix}-status.log") == "${bank}" ]]; then
            if [[ ${stale} == true ]]; then
                grep -q '"stale": true' "${runtime_dir}/${prefix}-policy.log" && return 0
            elif ! grep -q '"stale": true' "${runtime_dir}/${prefix}-policy.log"; then
                return 0
            fi
        fi
        /usr/bin/sleep 0.1
    done
    echo "IPv6 DNS state ${prefix} did not converge" >&2
    cat "${runtime_dir}/${prefix}-policy.log" >&2
    cat "${runtime_dir}/${prefix}-status.log" >&2
    return 1
}

wait_dns_state ::2 1 0 false initial
probe_denied ::1 18083 static-address-denied
probe_allowed ::1 18084 static-port-allowed
probe_denied 2001:db8::1234 18087 static-cidr-denied
probe_denied ::2 "${dns_port}" dns-initial-denied
probe_allowed ::3 "${dns_port}" dns-initial-other-allowed

write_hosts ::3
wait_dns_state ::3 2 1 false changed
if grep -q '"::2"' "${runtime_dir}/changed-policy.log"; then
    echo "old IPv6 DNS address remained after refresh" >&2
    exit 1
fi
probe_allowed ::2 "${dns_port}" dns-changed-old-allowed
probe_denied ::3 "${dns_port}" dns-changed-new-denied

write_hosts
wait_dns_state ::3 2 1 true stale
probe_denied ::3 "${dns_port}" dns-stale-denied

write_hosts ::2
wait_dns_state ::2 3 0 false recovered
if grep -q '"::3"' "${runtime_dir}/recovered-policy.log"; then
    echo "stale IPv6 DNS address remained after recovery" >&2
    exit 1
fi
probe_denied ::2 "${dns_port}" dns-recovered-new-denied
probe_allowed ::3 "${dns_port}" dns-recovered-old-allowed

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=
wait "${audit_pid}" || true
audit_pid=

grep -q 'DENY.*00000000000000000000000000000001:18083' "${runtime_dir}/audit.log"
grep -q 'ALLOW.*00000000000000000000000000000001:18084' "${runtime_dir}/audit.log"
grep -q 'DENY.*20010db8000000000000000000001234:18087' "${runtime_dir}/audit.log"
grep -q 'DNS policy refresh: generation=1 version=2' "${runtime_dir}/daemon.log"
grep -q 'DNS policy refresh: generation=2 version=3' "${runtime_dir}/daemon.log"

echo "IPv6 integration passed: address/CIDR/port priority and AAAA ::2 -> ::3 -> stale(::3) -> ::2"
