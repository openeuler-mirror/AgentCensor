#!/usr/bin/env bash
set -euo pipefail

if [[ $(id -u) -eq 0 ]]; then
  echo "run this test as the non-root Agent user with passwordless sudo for censorfs-mounter" >&2
  exit 2
fi
if [[ $(uname -m) != aarch64 ]]; then
  echo "this smoke test requires an AArch64 server" >&2
  exit 2
fi
. /etc/os-release
os_identity="${ID:-} ${ID_LIKE:-} ${NAME:-}"
if [[ ${os_identity,,} != *openeuler* ]]; then
  echo "this smoke test targets openEuler" >&2
  exit 2
fi
kernel_major=$(uname -r | cut -d. -f1)
kernel_minor=$(uname -r | cut -d. -f2)
if (( kernel_major < 6 || (kernel_major == 6 && kernel_minor < 6) )); then
  echo "Linux 6.6 or newer is required" >&2
  exit 2
fi
command -v jq >/dev/null
sudo -n true
[[ -c /dev/fuse ]]

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${CENSORFS_BIN_DIR:-"$repo_root/target/release"}
for program in censorfsd censorfsctl censorfs-mounter; do
  [[ -x "$bin/$program" ]]
done

test_parent=${CENSORFS_TEST_PARENT:-/var/tmp}
test_parent=$(cd -- "$test_parent" && pwd -P)
test_root=$(mktemp -d "$test_parent/censorfs-multiview.XXXXXX")
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
    if [[ $(dirname -- "$test_root") == "$test_parent" && $(basename -- "$test_root") == censorfs-multiview.* ]]; then
      rm -rf -- "$test_root"
    fi
  else
    echo "failed test data preserved at $test_root" >&2
  fi
}
trap cleanup EXIT

mkdir "$test_root/import"
printf initial >"$test_root/import/shared.txt"
store="$test_root/.censorfs"
socket="$test_root/control.sock"
"$bin/censorfsctl" init \
  --storage-root "$store" --import-root "$test_root/import" --branch main >/dev/null
"$bin/censorfsd" --storage-root "$store" --socket "$socket" >"$test_root/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 100); do [[ -S "$socket" ]] && break; sleep 0.05; done
[[ -S "$socket" ]]

base_json=$("$bin/censorfsctl" --socket "$socket" branch-head main)
base_generation=$(jq -r .generation_id <<<"$base_json")
branches=(agent-a agent-b agent-c)
values=(alpha beta gamma)
declare -a txs tickets candidates inodes published

for branch in "${branches[@]}"; do
  "$bin/censorfsctl" --socket "$socket" create-branch "$branch" "$base_generation" >/dev/null
done

uid=$(id -u)
gid=$(id -g)
for index in "${!branches[@]}"; do
  branch=${branches[$index]}
  value=${values[$index]}
  txs[$index]=$("$bin/censorfsctl" --socket "$socket" begin-tx | jq -r .tx_id)
  tickets[$index]=$(
    "$bin/censorfsctl" --socket "$socket" begin-ticket "${txs[$index]}" "$branch" | jq -r .ticket_id
  )
  view=$(
    "$bin/censorfsctl" --socket "$socket" open-ticket "${tickets[$index]}" | jq -r .view_id
  )
  inodes[$index]=$(
    timeout 30 sudo -n "$bin/censorfs-mounter" \
      --socket "$socket" --view-id "$view" --uid "$uid" --gid "$gid" -- \
      /bin/sh -c '
        test "$(id -u):$(id -g)" = "$1:$2"
        test "$(stat -c %u:%g /workspace/shared.txt)" = "$1:$2"
        test "$(cat /workspace/shared.txt)" = initial
        printf %s "$3" >/workspace/shared.txt
        stat -c %i /workspace/shared.txt
      ' sh "$uid" "$gid" "$value"
  )
done

[[ $(printf '%s\n' "${inodes[@]}" | sort -u | wc -l) -eq 3 ]]

# Ticket writes are still private: every stable branch remains at the common base.
for branch in "${branches[@]}"; do
  view=$("$bin/censorfsctl" --socket "$socket" open-branch "$branch" | jq -r .view_id)
  timeout 30 sudo -n "$bin/censorfs-mounter" \
    --socket "$socket" --view-id "$view" --uid "$uid" --gid "$gid" --read-only -- \
    /bin/sh -c 'test "$(cat /workspace/shared.txt)" = initial'
done

for index in "${!branches[@]}"; do
  candidates[$index]=$(
    "$bin/censorfsctl" --socket "$socket" prepare "${tickets[$index]}" | jq -r .candidate.candidate_id
  )
  receipt=$(
    "$bin/censorfsctl" --socket "$socket" publish \
      "${candidates[$index]}" "$base_generation" 0 "publish-${branches[$index]}"
  )
  published[$index]=$(jq -r .new_generation <<<"$receipt")
done

[[ $(printf '%s\n' "${published[@]}" | sort -u | wc -l) -eq 3 ]]
for index in "${!branches[@]}"; do
  view=$("$bin/censorfsctl" --socket "$socket" open-branch "${branches[$index]}" | jq -r .view_id)
  timeout 30 sudo -n "$bin/censorfs-mounter" \
    --socket "$socket" --view-id "$view" --uid "$uid" --gid "$gid" --read-only -- \
    /bin/sh -c '
      test "$(cat /workspace/shared.txt)" = "$1"
      ! (printf forbidden >/workspace/must-not-exist) 2>/dev/null
    ' sh "${values[$index]}"
done

# Roll back agent-a by publishing a new rollback generation, never by moving Head backward.
rollback_tx=$("$bin/censorfsctl" --socket "$socket" begin-tx | jq -r .tx_id)
current_json=$("$bin/censorfsctl" --socket "$socket" branch-head agent-a)
current_generation=$(jq -r .generation_id <<<"$current_json")
current_seq=$(jq -r .head_seq <<<"$current_json")
rollback_candidate=$(
  "$bin/censorfsctl" --socket "$socket" rollback \
    "$rollback_tx" agent-a "$base_generation" "$current_generation" "$current_seq" |
    jq -r .candidate.candidate_id
)
rollback_receipt=$(
  "$bin/censorfsctl" --socket "$socket" publish \
    "$rollback_candidate" "$current_generation" "$current_seq" rollback-agent-a
)
rollback_generation=$(jq -r .new_generation <<<"$rollback_receipt")
[[ "$rollback_generation" != "$base_generation" ]]
[[ $("$bin/censorfsctl" --socket "$socket" generation "$rollback_generation" | jq -r .kind) == Rollback ]]
view=$("$bin/censorfsctl" --socket "$socket" open-branch agent-a | jq -r .view_id)
timeout 30 sudo -n "$bin/censorfs-mounter" \
  --socket "$socket" --view-id "$view" --uid "$uid" --gid "$gid" --read-only -- \
  /bin/sh -c 'test "$(cat /workspace/shared.txt)" = initial'

passed=1
echo "three isolated views, distinct inodes, independent publishing and rollback passed"
