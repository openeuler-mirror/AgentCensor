#!/usr/bin/env bash
set -euo pipefail

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

"${repo_dir}/target/debug/censorguardd" \
    --config "${lab_dir}/policy.yaml" \
    --bpf-object "${repo_dir}/bpf/enforce.bpf.o" \
    --ctl-sock "${lab_dir}/ctl.sock" \
    --event-sock "${lab_dir}/events.sock" \
    --track-pid "${victim_pid}" \
    --domain lab-agent \
    --hold-seconds 3 >"${lab_dir}/daemon.log" 2>&1 &
daemon_pid=$!

ready=false
for _ in $(seq 1 200); do
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

