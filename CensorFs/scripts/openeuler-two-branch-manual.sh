#!/usr/bin/env bash
set -euo pipefail

# Interactive by default. Set CENSORFS_PAUSE=0 for unattended execution and
# CENSORFS_KEEP_TEST_DATA=0 to remove the generated /var/tmp instance on success.
pause_enabled=${CENSORFS_PAUSE:-1}
keep_data=${CENSORFS_KEEP_TEST_DATA:-1}

step() {
  printf '\n==> %s\n' "$1"
  if [[ "$pause_enabled" == 1 && -t 0 ]]; then
    read -r -p "Press Enter to continue... "
  fi
}

if [[ $(id -u) -eq 0 ]]; then
  echo "Run this script as the non-root Agent user; only censorfs-mounter uses sudo." >&2
  exit 2
fi
command -v jq >/dev/null
command -v timeout >/dev/null
sudo -v
[[ -c /dev/fuse ]]

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${CENSORFS_BIN_DIR:-"$repo_root/target/release"}
for program in censorfsd censorfsctl censorfs-mounter; do
  if [[ ! -x "$bin/$program" ]]; then
    echo "missing executable: $bin/$program" >&2
    echo "build first with: cargo +1.82.0 build --workspace --release --locked" >&2
    exit 2
  fi
done

test_root=$(mktemp -d /var/tmp/censorfs-two-branch.XXXXXX)
store="$test_root/.censorfs"
socket="$test_root/control.sock"
daemon_pid=
passed=0

cleanup() {
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  if [[ $passed -eq 1 && "$keep_data" == 0 ]]; then
    case "$test_root" in
      /var/tmp/censorfs-two-branch.*) rm -rf -- "$test_root" ;;
    esac
  else
    echo "test data: $test_root" >&2
    [[ $passed -eq 1 ]] || echo "daemon log: $test_root/daemon.log" >&2
  fi
}
trap cleanup EXIT

ctl() {
  "$bin/censorfsctl" --socket "$socket" "$@"
}

mount_ticket() {
  local view=$1
  shift
  timeout 30 sudo "$bin/censorfs-mounter" \
    --socket "$socket" --view-id "$view" \
    --uid "$(id -u)" --gid "$(id -g)" -- "$@"
}

mount_stable() {
  local view=$1
  shift
  timeout 30 sudo "$bin/censorfs-mounter" \
    --socket "$socket" --view-id "$view" \
    --uid "$(id -u)" --gid "$(id -g)" --read-only -- "$@"
}

step "Initialize a common base Generation"
mkdir "$test_root/import"
printf 'base\n' >"$test_root/import/shared.txt"
mkdir "$test_root/import/common"
printf 'common\n' >"$test_root/import/common/file.txt"
"$bin/censorfsctl" init \
  --storage-root "$store" --import-root "$test_root/import" --branch main >/dev/null
"$bin/censorfsd" --storage-root "$store" --socket "$socket" >"$test_root/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 100); do [[ -S "$socket" ]] && break; sleep 0.05; done
[[ -S "$socket" ]]

base_json=$(ctl branch-head main)
base_generation=$(jq -r .generation_id <<<"$base_json")
base_seq=$(jq -r .head_seq <<<"$base_json")
printf 'base generation=%s head_seq=%s\n' "$base_generation" "$base_seq"

step "Create branch-a and branch-b from the same Generation"
ctl create-branch branch-a "$base_generation" >/dev/null
ctl create-branch branch-b "$base_generation" >/dev/null
head_a=$(ctl branch-head branch-a)
head_b=$(ctl branch-head branch-b)
[[ $(jq -r .generation_id <<<"$head_a") == "$base_generation" ]]
[[ $(jq -r .generation_id <<<"$head_b") == "$base_generation" ]]
[[ $(jq -r .head_seq <<<"$head_a") -eq 0 ]]
[[ $(jq -r .head_seq <<<"$head_b") -eq 0 ]]
printf '%s\n%s\n' "$head_a" "$head_b"

step "Create two private Ticket Views and write different content"
tx_a=$(ctl begin-tx | jq -r .tx_id)
tx_b=$(ctl begin-tx | jq -r .tx_id)
ticket_a=$(ctl begin-ticket "$tx_a" branch-a | jq -r .ticket_id)
ticket_b=$(ctl begin-ticket "$tx_b" branch-b | jq -r .ticket_id)
view_a=$(ctl open-ticket "$ticket_a" | jq -r .view_id)
view_b=$(ctl open-ticket "$ticket_b" | jq -r .view_id)

inode_a=$(
  mount_ticket "$view_a" /bin/sh -c '
    test "$(cat /workspace/shared.txt)" = base
    printf "content-a\n" >/workspace/shared.txt
    mkdir /workspace/only-a
    printf "a\n" >/workspace/only-a/file.txt
    stat -c %i /workspace/shared.txt
  '
)
inode_b=$(
  mount_ticket "$view_b" /bin/sh -c '
    test "$(cat /workspace/shared.txt)" = base
    printf "content-b\n" >/workspace/shared.txt
    mkdir /workspace/only-b
    printf "b\n" >/workspace/only-b/file.txt
    stat -c %i /workspace/shared.txt
  '
)
[[ "$inode_a" != "$inode_b" ]]
printf 'branch-a inode=%s, branch-b inode=%s\n' "$inode_a" "$inode_b"

step "Verify the two Ticket Uppers do not leak into each other"
inspect_a=$(ctl open-ticket "$ticket_a" | jq -r .view_id)
inspect_b=$(ctl open-ticket "$ticket_b" | jq -r .view_id)
mount_ticket "$inspect_a" /bin/sh -c '
  test "$(cat /workspace/shared.txt)" = content-a
  test -f /workspace/only-a/file.txt
  test ! -e /workspace/only-b
'
mount_ticket "$inspect_b" /bin/sh -c '
  test "$(cat /workspace/shared.txt)" = content-b
  test -f /workspace/only-b/file.txt
  test ! -e /workspace/only-a
'
echo "private Upper isolation passed"

step "Verify stable Branch Views still show the base before Publish"
stable_a=$(ctl open-branch branch-a | jq -r .view_id)
stable_b=$(ctl open-branch branch-b | jq -r .view_id)
for view in "$stable_a" "$stable_b"; do
  mount_stable "$view" /bin/sh -c '
    test "$(cat /workspace/shared.txt)" = base
    test ! -e /workspace/only-a
    test ! -e /workspace/only-b
  '
done
echo "unpublished writes are invisible to stable Views"

step "Prepare both Tickets"
candidate_a=$(ctl prepare "$ticket_a" | jq -r .candidate.candidate_id)
candidate_b=$(ctl prepare "$ticket_b" | jq -r .candidate.candidate_id)
printf 'candidate-a=%s\ncandidate-b=%s\n' "$candidate_a" "$candidate_b"

step "Publish branch-a only and prove branch-b Head does not move"
receipt_a=$(ctl publish "$candidate_a" "$base_generation" 0 publish-a)
generation_a=$(jq -r .new_generation <<<"$receipt_a")
head_a=$(ctl branch-head branch-a)
head_b=$(ctl branch-head branch-b)
[[ $(jq -r .generation_id <<<"$head_a") == "$generation_a" ]]
[[ $(jq -r .head_seq <<<"$head_a") -eq 1 ]]
[[ $(jq -r .generation_id <<<"$head_b") == "$base_generation" ]]
[[ $(jq -r .head_seq <<<"$head_b") -eq 0 ]]
printf 'branch-a=%s\nbranch-b=%s\n' "$head_a" "$head_b"

step "Publish branch-b independently"
receipt_b=$(ctl publish "$candidate_b" "$base_generation" 0 publish-b)
generation_b=$(jq -r .new_generation <<<"$receipt_b")
[[ "$generation_a" != "$generation_b" ]]
printf 'generation-a=%s\ngeneration-b=%s\n' "$generation_a" "$generation_b"

step "Verify the final stable Views and their read-only property"
final_a=$(ctl open-branch branch-a | jq -r .view_id)
final_b=$(ctl open-branch branch-b | jq -r .view_id)
mount_stable "$final_a" /bin/sh -c '
  test "$(cat /workspace/shared.txt)" = content-a
  test -f /workspace/only-a/file.txt
  test ! -e /workspace/only-b
  ! (printf forbidden >/workspace/must-not-exist) 2>/dev/null
'
mount_stable "$final_b" /bin/sh -c '
  test "$(cat /workspace/shared.txt)" = content-b
  test -f /workspace/only-b/file.txt
  test ! -e /workspace/only-a
  ! (printf forbidden >/workspace/must-not-exist) 2>/dev/null
'

step "Show the manifest-level difference between the two Generations"
ctl diff "$generation_a" "$generation_b"

ctl close-tx "$tx_a" >/dev/null
ctl close-tx "$tx_b" >/dev/null
passed=1
echo
echo "PASS: two branches have isolated Ticket Uppers, inodes, Heads and published Generations"
