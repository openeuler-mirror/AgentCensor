#!/usr/bin/env bash
set -euo pipefail

[[ "$(uname -s)" == Linux ]] || { echo "Linux is required" >&2; exit 2; }
[[ "$(uname -m)" == aarch64 ]] || { echo "AArch64 is required" >&2; exit 2; }
command -v censorfs >/dev/null
command -v censorfs-mounter >/dev/null
command -v jq >/dev/null
command -v rg >/dev/null

parent=${CENSORFS_DEMO_PARENT:-/data}
root="$parent/censorfs-parallel-worlds-smoke-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$root/import"
printf 'initial\n' >"$root/import/shared.txt"
export CENSORFS_STORAGE_ROOT="$root/.censorfs"
export CENSORFS_SOCKET="$root/control.sock"

censorfs init --import-root "$root/import" --branch main
censorfs daemon >"$root/daemon.log" 2>&1 &
daemon_pid=$!
cleanup() {
  kill "$daemon_pid" 2>/dev/null || true
  wait "$daemon_pid" 2>/dev/null || true
}
trap cleanup EXIT
for _ in $(seq 1 100); do
  [[ -S "$CENSORFS_SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$CENSORFS_SOCKET" ]] || { echo "daemon did not start" >&2; exit 1; }

head_json=$(censorfs --json head main)
generation=$(jq -r .generation_id <<<"$head_json")
head_seq=$(jq -r .head_seq <<<"$head_json")
run_id="aarch64-smoke"

declare -a tickets views candidates results pids
for index in 0 1 2; do
  variant="world-$((index + 1))"
  opened=$(censorfs --request-id "$(cat /proc/sys/kernel/random/uuid)" variant-open \
    --branch main --expected-generation "$generation" --expected-head-seq "$head_seq" \
    --run "$run_id" --variant "$variant")
  tickets[$index]=$(jq -r .result.ticket.ticket_id <<<"$opened")
  views[$index]=$(jq -r .result.view.view_id <<<"$opened")
  uid=$(jq -r .result.view.owner_uid <<<"$opened")
  gid=$(jq -r .result.view.owner_gid <<<"$opened")
  WORLD_VALUE="candidate-$((index + 1))" censorfs-mounter \
    --socket "$CENSORFS_SOCKET" --view-id "${views[$index]}" --uid "$uid" --gid "$gid" -- \
    sh -c 'printf "%s\n" "$WORLD_VALUE" >shared.txt; test "$(cat shared.txt)" = "$WORLD_VALUE"; rg -q "$WORLD_VALUE" shared.txt' &
  pids[$index]=$!
done
for pid in "${pids[@]}"; do wait "$pid"; done

test "$(censorfs cat --branch main /shared.txt)" = initial

for index in 0 1 2; do
  variant="world-$((index + 1))"
  prepared=$(censorfs --request-id "$(cat /proc/sys/kernel/random/uuid)" variant-prepare \
    --ticket "${tickets[$index]}" --view "${views[$index]}" \
    --run "$run_id" --variant "$variant")
  candidates[$index]=$(jq -r .result.candidate.candidate_id <<<"$prepared")
  results[$index]=$(jq -r .result.generation.generation_id <<<"$prepared")
  candidate_view=$(censorfs candidate-view-open --candidate "${candidates[$index]}")
  validation_view=$(jq -r .result.view_id <<<"$candidate_view")
  validation_uid=$(jq -r .result.owner_uid <<<"$candidate_view")
  validation_gid=$(jq -r .result.owner_gid <<<"$candidate_view")
  EXPECTED="candidate-$((index + 1))" censorfs-mounter \
    --socket "$CENSORFS_SOCKET" --view-id "$validation_view" \
    --uid "$validation_uid" --gid "$validation_gid" --read-only -- \
    sh -c 'test "$(cat shared.txt)" = "$EXPECTED"; ! printf forbidden >must-fail.txt'
  censorfs view-close --view "$validation_view" >/dev/null
done

censorfs --request-id "$(cat /proc/sys/kernel/random/uuid)" variant-publish \
  --candidate "${candidates[1]}" --expected-generation "$generation" --expected-head-seq "$head_seq" \
  --decision-id aarch64-smoke-winner --run "$run_id" --variant world-2 >/dev/null

set +e
stale=$(censorfs --request-id "$(cat /proc/sys/kernel/random/uuid)" variant-publish \
  --candidate "${candidates[0]}" --expected-generation "$generation" --expected-head-seq "$head_seq" \
  --decision-id must-be-stale --run "$run_id" --variant world-1)
stale_code=$?
set -e
[[ $stale_code -ne 0 ]]
test "$(jq -r .code <<<"$stale")" = HeadChanged

for index in 0 2; do
  censorfs --request-id "$(cat /proc/sys/kernel/random/uuid)" variant-abort \
    --ticket "${tickets[$index]}" --view "${views[$index]}" \
    --run "$run_id" --variant "world-$((index + 1))" >/dev/null
done

test "$(censorfs cat --branch main /shared.txt)" = candidate-2
test "$(censorfs --json head main | jq -r .head_seq)" -eq "$((head_seq + 1))"
echo "Parallel Worlds AArch64 FUSE smoke passed. Artifacts: $root"
