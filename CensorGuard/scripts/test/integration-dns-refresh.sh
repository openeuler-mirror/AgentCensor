#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-dns-refresh
hostname=dynamic.Censorguard.invalid
port=18082
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill -KILL "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

for tool in cc unshare mount; do
    command -v "${tool}" >/dev/null
done

install -d -m 0700 "${runtime_dir}"
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/connect_probe.c" \
    -o "${runtime_dir}/connect-probe"
printf 'hosts: files\n' >"${runtime_dir}/nsswitch.conf"

write_hosts() {
    local address=${1:-}
    printf '127.0.0.1 localhost\n' >"${runtime_dir}/hosts"
    if [[ -n ${address} ]]; then
        printf '%s %s\n' "${address}" "${hostname}" >>"${runtime_dir}/hosts"
    fi
}
write_hosts 127.0.0.2

cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  dynamic-net:
    rules:
      - net deny ${hostname}:${port}
domains:
  - name: dns-domain
    group: dynamic-net
YAML
: >"${runtime_dir}/daemon.log"

unshare --mount --propagation private /bin/bash -c '
    set -e
    mount --bind "$1" /etc/hosts
    mount --bind "$2" /etc/nsswitch.conf
    shift 2
    exec "$@"
' Censorguard-dns-refresh \
    "${runtime_dir}/hosts" "${runtime_dir}/nsswitch.conf" \
    "${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --dns-refresh-seconds 1 \
    --duration 30 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
if ! grep -q '^\[READY\]' "${runtime_dir}/daemon.log"; then
    cat "${runtime_dir}/daemon.log" >&2
    exit 1
fi

status_value() {
    local field=$1
    local file=$2
    sed -n "s/.*\"${field}\": \([0-9][0-9]*\).*/\1/p" "${file}"
}

probe_denied() {
    local address=$1
    local name=$2
    set +e
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain dns-domain -- "${runtime_dir}/connect-probe" \
        "${address}" "${port}" >"${runtime_dir}/${name}.log" 2>&1
    local result=$?
    set -e
    [[ ${result} -ne 0 ]]
    grep -q 'Operation not permitted' "${runtime_dir}/${name}.log"
}

probe_allowed() {
    local address=$1
    local name=$2
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain dns-domain -- "${runtime_dir}/connect-probe" \
        "${address}" "${port}" >"${runtime_dir}/${name}.log" 2>&1
    grep -q 'Connection refused' "${runtime_dir}/${name}.log"
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
    echo "DNS state ${prefix} did not converge" >&2
    cat "${runtime_dir}/${prefix}-policy.log" >&2
    cat "${runtime_dir}/${prefix}-status.log" >&2
    return 1
}

wait_dns_state 127.0.0.2 1 0 false initial
probe_denied 127.0.0.2 initial-denied
probe_allowed 127.0.0.3 initial-allowed

write_hosts 127.0.0.3
wait_dns_state 127.0.0.3 2 1 false changed
if grep -q '"127.0.0.2"' "${runtime_dir}/changed-policy.log"; then
    echo "old DNS address remained after refresh" >&2
    exit 1
fi
probe_allowed 127.0.0.2 changed-old-allowed
probe_denied 127.0.0.3 changed-new-denied

write_hosts
wait_dns_state 127.0.0.3 2 1 true stale
probe_denied 127.0.0.3 stale-cache-denied

write_hosts 127.0.0.2
wait_dns_state 127.0.0.2 3 0 false recovered
if grep -q '"127.0.0.3"' "${runtime_dir}/recovered-policy.log"; then
    echo "stale DNS address remained after recovery" >&2
    exit 1
fi
probe_denied 127.0.0.2 recovered-new-denied
probe_allowed 127.0.0.3 recovered-old-allowed

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=
grep -q 'DNS policy refresh: generation=1 version=2' "${runtime_dir}/daemon.log"
grep -q 'DNS policy refresh: generation=2 version=3' "${runtime_dir}/daemon.log"

echo "DNS refresh integration passed: .2 -> .3 -> stale(.3) -> .2"
