#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Linux ]]; then
  echo "this smoke test requires Linux" >&2
  exit 2
fi
command -v jq >/dev/null
command -v node >/dev/null
command -v bwrap >/dev/null
command -v setpriv >/dev/null
[[ -c /dev/fuse ]]

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${CENSORFS_BIN_DIR:-"$repo_root/target/release"}
runner="$repo_root/integrations/deepseek-harness/src/runner-process.js"
for program in censorfsd censorfsctl censorfs-mounter; do
  [[ -x "$bin/$program" ]]
done
[[ -f "$runner" ]]

if [[ $(id -u) -eq 0 ]]; then
  mounter=("$bin/censorfs-mounter")
  uid=${CENSORFS_AGENT_UID:-65534}
  gid=${CENSORFS_AGENT_GID:-65534}
  if [[ $uid -eq 0 || $gid -eq 0 ]]; then
    echo "root smoke requires non-zero CENSORFS_AGENT_UID/GID" >&2
    exit 2
  fi
  agent=(setpriv --reuid "$uid" --regid "$gid" --clear-groups)
else
  sudo -n true
  mounter=(sudo -n "$bin/censorfs-mounter")
  uid=$(id -u)
  gid=$(id -g)
  agent=()
fi
test_parent=${CENSORFS_TEST_PARENT:-/var/tmp}
test_parent=$(cd -- "$test_parent" && pwd -P)
test_root=$(mktemp -d "$test_parent/censorfs-namespace-runner.XXXXXX")
if [[ $(id -u) -eq 0 ]]; then chown "$uid:$gid" "$test_root"; fi
backing=$(stat -f -c %T "$test_root")
if [[ "$backing" != xfs && "$backing" != ext2/ext3 ]]; then
  echo "smoke root must be local XFS or ext4; found $backing" >&2
  exit 2
fi

daemon_pid=
passed=0
cleanup() {
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  if [[ $passed -eq 1 ]]; then
    rm -rf -- "$test_root"
  else
    echo "failed test data preserved at $test_root" >&2
  fi
}
trap cleanup EXIT

mkdir "$test_root/import"
printf initial >"$test_root/import/shared.txt"
store="$test_root/.censorfs"
socket="$test_root/control.sock"
"${agent[@]}" "$bin/censorfsctl" init --storage-root "$store" --import-root "$test_root/import" --branch main >/dev/null
"${agent[@]}" "$bin/censorfsd" --storage-root "$store" --socket "$socket" >"$test_root/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 100); do [[ -S "$socket" ]] && break; sleep 0.05; done
[[ -S "$socket" ]]

values=(alpha beta)
declare -a tickets views outputs candidates
for index in "${!values[@]}"; do
  tx=$("${agent[@]}" "$bin/censorfsctl" --socket "$socket" begin-tx | jq -r .tx_id)
  tickets[$index]=$("${agent[@]}" "$bin/censorfsctl" --socket "$socket" begin-ticket "$tx" main | jq -r .ticket_id)
  views[$index]=$("${agent[@]}" "$bin/censorfsctl" --socket "$socket" open-ticket "${tickets[$index]}" | jq -r .view_id)
  output="$test_root/runner-$index.jsonl"
  outputs[$index]=$output
  {
    printf '{"id":"health","method":"health","params":{}}\n'
    printf '{"id":"write","method":"fs.write","params":{"file_path":"/workspace/shared.txt","content":"%s"}}\n' "${values[$index]}"
    printf '%s\n' '{"id":"shell","method":"process.run","params":{"command":"test ! -e /home/yyy/censorfs; printf -- -shell >> /workspace/shared.txt; cat /workspace/shared.txt; printf :; stat -c %i /workspace/shared.txt","timeoutMs":5000}}'
    printf '%s\n' '{"id":"timeout","method":"process.run","params":{"command":"(sleep 1; touch /workspace/must-not-leak) & wait","timeoutMs":100}}'
    printf '%s\n' '{"id":"cleanup","method":"process.run","params":{"command":"sleep 2; test ! -e /workspace/must-not-leak","timeoutMs":5000}}'
    printf '%s\n' '{"id":"read","method":"fs.read","params":{"file_path":"/workspace/shared.txt"}}'
    printf '%s\n' '{"id":"shutdown","method":"shutdown","params":{}}'
  } | timeout 30 "${mounter[@]}" \
    --socket "$socket" --view-id "${views[$index]}" --uid "$uid" --gid "$gid" -- \
    "$(command -v node)" "$runner" --runner-id "runner-$index" --view-id "${views[$index]}" >"$output"

  [[ $(jq -r 'select(.id=="health") | .result.protocolVersion' "$output") == 1 ]]
  [[ $(jq -r 'select(.id=="health") | .result.runnerId' "$output") == "runner-$index" ]]
  [[ $(jq -r 'select(.id=="health") | .result.cwd' "$output") == /workspace ]]
  [[ $(jq -r 'select(.id=="health") | .result.viewId' "$output") == "${views[$index]}" ]]
  [[ $(jq -r 'select(.id=="shell") | .result.exitCode' "$output") == 0 ]]
  [[ $(jq -r 'select(.id=="timeout") | .result.timedOut' "$output") == true ]]
  [[ $(jq -r 'select(.id=="cleanup") | .result.exitCode' "$output") == 0 ]]
  [[ $(jq -r 'select(.id=="shell") | .result.stdout.text' "$output") == "${values[$index]}-shell:"* ]]
  [[ $(jq -r 'select(.id=="read") | .result.lines[0].text' "$output") == "${values[$index]}-shell" ]]
done

# The stable branch remains unchanged while both ticket overlays hold private writes.
stable_view=$("${agent[@]}" "$bin/censorfsctl" --socket "$socket" open-branch main | jq -r .view_id)
timeout 30 "${mounter[@]}" \
  --socket "$socket" --view-id "$stable_view" --uid "$uid" --gid "$gid" --read-only -- \
  /bin/sh -c 'test "$(cat /workspace/shared.txt)" = initial'

for index in "${!tickets[@]}"; do
  candidates[$index]=$("${agent[@]}" "$bin/censorfsctl" --socket "$socket" prepare "${tickets[$index]}" | jq -r .candidate.candidate_id)
done
[[ "${candidates[0]}" != "${candidates[1]}" ]]

passed=1
echo "two Namespace Runners used distinct real /workspace Views, isolated writes, foreground bash, and prepared Candidates"
