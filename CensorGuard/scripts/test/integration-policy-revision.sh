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
runtime_dir=/tmp/censorguard-policy-revision
daemon_pid=

cleanup() {
    [[ -n ${daemon_pid} ]] && kill "${daemon_pid}" 2>/dev/null || true
}
trap cleanup EXIT

install -d -m 0700 "${runtime_dir}"
rm -rf -- "${runtime_dir}/state"
: >"${runtime_dir}/daemon.log"

# Revision 1 must deny /usr/bin/id in lab-policy so the rollback assertions hold;
# config/base.yaml no longer carries that rule.
cat >"${runtime_dir}/policy-initial.yaml" <<'YAML'
groups:
  lab-policy:
    rules:
      - file deny /tmp/censorguard/protected.txt [read]
      - exec deny /usr/bin/id
domains:
  - name: revision-test
    group: lab-policy
  - name: revision-after-restart
    group: lab-policy
YAML
cat >"${runtime_dir}/policy-allow.yaml" <<'YAML'
groups:
  lab-policy:
    rules: []
domains:
  - name: revision-test
    group: lab-policy
  - name: revision-after-restart
    group: lab-policy
YAML

write_test_config "${runtime_dir}/config.yaml" "${runtime_dir}/policy-initial.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}" "${runtime_dir}/state"
write_test_snapshot "${runtime_dir}/config.yaml" "${runtime_dir}/policy-allow.yaml" "${runtime_dir}/allow-snapshot.yaml"
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 10 >"${runtime_dir}/daemon.log" 2>&1 &
daemon_pid=$!

for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon.log"

rpc="${repo_dir}/scripts/test/rpc-v2.py"
"${rpc}" --config "${runtime_dir}/config.yaml" --method get_capabilities \
    >"${runtime_dir}/capabilities.json"
grep -q 'launcher.peer-credentials' "${runtime_dir}/capabilities.json"
grep -q 'policy.read-current' "${runtime_dir}/capabilities.json"

"${rpc}" --config "${runtime_dir}/config.yaml" --method get_policy \
    >"${runtime_dir}/policy-initial.json"
grep -q '"revision": 1' "${runtime_dir}/policy-initial.json"
grep -q 'lab-policy' "${runtime_dir}/policy-initial.json"

"${rpc}" --config "${runtime_dir}/config.yaml" --method evaluate_intent \
    --params '{"group":"lab-policy","intents":[{"kind":"file","operation":"read","path":"/tmp/censorguard/protected.txt"},{"kind":"exec","executable":"/usr/bin/id","argv":["/usr/bin/id"]}]}' \
    >"${runtime_dir}/intent-before.json"
[[ $(grep -c '"allowed": false' "${runtime_dir}/intent-before.json") -eq 2 ]]

"${rpc}" --config "${runtime_dir}/config.yaml" --method validate_policy \
    --policy-file "${runtime_dir}/allow-snapshot.yaml" \
    >"${runtime_dir}/validate.json"
grep -q '"revision": 1' "${runtime_dir}/validate.json"

"${rpc}" --config "${runtime_dir}/config.yaml" --method apply_policy \
    --params '{"expected_revision":1,"idempotency_key":"apply-allow-v1"}' \
    --policy-file "${runtime_dir}/allow-snapshot.yaml" \
    >"${runtime_dir}/apply.json"
grep -q '"revision": 2' "${runtime_dir}/apply.json"

"${rpc}" --config "${runtime_dir}/config.yaml" --method get_policy \
    >"${runtime_dir}/policy-applied.json"
grep -q '"revision": 2' "${runtime_dir}/policy-applied.json"
grep -q 'rules: \[\]' "${runtime_dir}/policy-applied.json"

"${rpc}" --config "${runtime_dir}/config.yaml" --method evaluate_intent \
    --params '{"group":"lab-policy","intents":[{"kind":"exec","executable":"/usr/bin/id","argv":["/usr/bin/id"]}]}' \
    >"${runtime_dir}/intent-after.json"
grep -q '"allowed": true' "${runtime_dir}/intent-after.json"

set +e
"${rpc}" --config "${runtime_dir}/config.yaml" --method apply_policy \
    --params '{"expected_revision":1,"idempotency_key":"stale-writer"}' \
    --policy-file "${repo_dir}/config/base.yaml" \
    >"${runtime_dir}/conflict.json"
conflict_rc=$?
set -e
[[ ${conflict_rc} -ne 0 ]]
grep -q 'revision_conflict' "${runtime_dir}/conflict.json"
grep -q '"current_revision": 2' "${runtime_dir}/conflict.json"

"${repo_dir}/target/debug/censorguardctl" \
    --config "${runtime_dir}/config.yaml" spawn --domain revision-test -- \
    /usr/bin/id >"${runtime_dir}/allowed.log"
grep -q 'uid=0' "${runtime_dir}/allowed.log"

kill "${daemon_pid}"
wait "${daemon_pid}" || true
daemon_pid=
: >"${runtime_dir}/daemon-restart.log"
# Restore dynamic policy using the same immutable startup configuration.
"${repo_dir}/target/debug/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" \
    --duration 8 >"${runtime_dir}/daemon-restart.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 1200); do
    grep -q '^\[READY\]' "${runtime_dir}/daemon-restart.log" && break
    /usr/bin/sleep 0.05
done
grep -q '^\[READY\]' "${runtime_dir}/daemon-restart.log"

"${rpc}" --config "${runtime_dir}/config.yaml" --method get_policy \
    >"${runtime_dir}/policy-restarted.json"
grep -q '"revision": 2' "${runtime_dir}/policy-restarted.json"
grep -q 'rules: \[\]' "${runtime_dir}/policy-restarted.json"

"${repo_dir}/target/debug/censorguardctl" \
    --config "${runtime_dir}/config.yaml" spawn --domain revision-after-restart -- \
    /usr/bin/id >"${runtime_dir}/allowed-after-restart.log"
grep -q 'uid=0' "${runtime_dir}/allowed-after-restart.log"

"${rpc}" --config "${runtime_dir}/config.yaml" --method rollback_policy \
    --params '{"revision":1,"expected_revision":2,"idempotency_key":"rollback-to-v1"}' \
    >"${runtime_dir}/rollback.json"
grep -q '"revision": 3' "${runtime_dir}/rollback.json"

"${rpc}" --config "${runtime_dir}/config.yaml" --method get_policy \
    >"${runtime_dir}/policy-rolled-back.json"
grep -q '"revision": 3' "${runtime_dir}/policy-rolled-back.json"
grep -q '/usr/bin/id' "${runtime_dir}/policy-rolled-back.json"

set +e
"${repo_dir}/target/debug/censorguardctl" \
    --config "${runtime_dir}/config.yaml" spawn --domain revision-test -- \
    /usr/bin/id >"${runtime_dir}/denied.log" 2>&1
denied_rc=$?
set -e
[[ ${denied_rc} -ne 0 ]]

wait "${daemon_pid}"
daemon_pid=
echo "policy revision integration passed: read-current, validate, conflict, restart consistency, revision 2 apply, revision 3 rollback"
