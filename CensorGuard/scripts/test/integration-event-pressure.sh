#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=/tmp/censorguard-event-pressure
protected_file=${runtime_dir}/protected.txt
daemon_pid=
drain_pid=
slow_pid=

cleanup() {
    for pid in "${slow_pid}" "${drain_pid}" "${daemon_pid}"; do
        [[ -n ${pid} ]] && kill -KILL "${pid}" 2>/dev/null || true
    done
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
printf 'secret\n' >"${protected_file}"
cc -O2 -Wall -Wextra -Werror "${repo_dir}/tools/event_stress_probe.c" \
    -o "${runtime_dir}/event-stress-probe"
cat >"${runtime_dir}/policy.yaml" <<YAML
groups:
  observed:
    rules:
      - file deny ${protected_file} [read]
domains:
  - name: event-domain
    group: observed
YAML
: >"${runtime_dir}/daemon.log"
: >"${runtime_dir}/drain.log"
: >"${runtime_dir}/slow.log"

"${repo_dir}/target/debug/censorguardd" \
    --config "${runtime_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${runtime_dir}/ctl.sock" \
    --event-sock "${runtime_dir}/events.sock" \
    --launch-sock "${runtime_dir}/launch.sock" \
    --duration 45 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

# ALLOW auditing is a runtime switch now: enable global file allow auditing
# (the daemon internally runs it at full rate).
"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    set --audit-file on >"${runtime_dir}/set-audit.log"

"${runtime_dir}/event-stress-probe" drain "${runtime_dir}/events.sock" \
    >"${runtime_dir}/drain.log" 2>&1 &
drain_pid=$!
"${runtime_dir}/event-stress-probe" slow "${runtime_dir}/events.sock" 40 \
    >"${runtime_dir}/slow.log" 2>&1 &
slow_pid=$!
for _ in $(seq 1 200); do
    if grep -q '^connected$' "${runtime_dir}/drain.log" && \
        grep -q '^connected$' "${runtime_dir}/slow.log"; then
        break
    fi
    /usr/bin/sleep 0.01
done
grep -q '^connected$' "${runtime_dir}/drain.log"
grep -q '^connected$' "${runtime_dir}/slow.log"

status_value() {
    local field=$1
    local file=$2
    sed -n "s/.*\"${field}\": \([0-9][0-9]*\).*/\1/p" "${file}"
}

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/before.log"
[[ $(status_value event_subscribers "${runtime_dir}/before.log") == 2 ]]

kernel_dropped=0
reader_dropped=0
subscriber_dropped=0
generated=0
for count in 50000 100000 250000 500000; do
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain event-domain -- "${runtime_dir}/event-stress-probe" \
        generate /etc/hosts "${count}" >"${runtime_dir}/generate-${count}.log" 2>&1
    generated=$((generated + count))
    /usr/bin/sleep 0.2
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        status >"${runtime_dir}/pressure.log"
    kernel_dropped=$(status_value event_kernel_dropped "${runtime_dir}/pressure.log")
    reader_dropped=$(status_value event_reader_dropped "${runtime_dir}/pressure.log")
    subscriber_dropped=$(status_value event_subscriber_dropped \
        "${runtime_dir}/pressure.log")
    if [[ ${kernel_dropped} -gt 0 && ${reader_dropped} -gt 0 && \
        ${subscriber_dropped} -gt 0 ]]; then
        break
    fi
done

if [[ ${kernel_dropped} -eq 0 || ${reader_dropped} -eq 0 || \
    ${subscriber_dropped} -eq 0 ]]; then
    echo "pressure did not trigger all three drop layers after ${generated} events" >&2
    cat "${runtime_dir}/pressure.log" >&2
    exit 1
fi
[[ $(status_value event_subscribers "${runtime_dir}/pressure.log") == 2 ]]

# Let the healthy subscriber drain its bounded queue, then send several sentinel DENY events.
/usr/bin/sleep 1
for _ in 1 2 3; do
    set +e
    "${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
        spawn --domain event-domain -- /usr/bin/cat "${protected_file}" \
        >/dev/null 2>&1
    sentinel_rc=$?
    set -e
    [[ ${sentinel_rc} -ne 0 ]]
done

"${repo_dir}/target/debug/censorguardctl" --socket "${runtime_dir}/ctl.sock" \
    status >"${runtime_dir}/after.log"
[[ $(status_value event_kernel_dropped "${runtime_dir}/after.log") -ge ${kernel_dropped} ]]
[[ $(status_value event_reader_dropped "${runtime_dir}/after.log") -ge ${reader_dropped} ]]
[[ $(status_value event_subscriber_dropped "${runtime_dir}/after.log") \
    -ge ${subscriber_dropped} ]]

kill -TERM "${daemon_pid}"
wait "${daemon_pid}"
daemon_pid=
wait "${drain_pid}"
drain_pid=
kill -TERM "${slow_pid}" 2>/dev/null || true
wait "${slow_pid}" 2>/dev/null || true
slow_pid=

if ! grep -Eq 'denied=[1-9][0-9]*' "${runtime_dir}/drain.log"; then
    echo "healthy subscriber did not receive a sentinel DENY after pressure" >&2
    cat "${runtime_dir}/drain.log" >&2
    exit 1
fi

echo "event pressure integration passed: generated=${generated} kernel_dropped=${kernel_dropped} reader_dropped=${reader_dropped} subscriber_dropped=${subscriber_dropped}"
