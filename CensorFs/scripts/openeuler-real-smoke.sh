#!/usr/bin/env bash
set -euo pipefail

if [[ $(id -u) -ne 0 ]]; then
  echo "run as root on an openEuler target" >&2
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
command -v jq >/dev/null
[[ -c /dev/fuse ]]

kernel_major=$(uname -r | cut -d. -f1)
kernel_minor=$(uname -r | cut -d. -f2)
if (( kernel_major < 6 || (kernel_major == 6 && kernel_minor < 6) )); then
  echo "Linux 6.6 or newer is required" >&2
  exit 2
fi

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${CENSORFS_BIN_DIR:-"$repo_root/target/release"}
for program in censorfsd censorfsctl censorfs-mounter; do
  if [[ ! -x "$bin/$program" ]]; then
    echo "missing release executable: $bin/$program" >&2
    echo "run scripts/build-openeuler-aarch64.sh before this smoke test" >&2
    exit 2
  fi
done
test_parent=${CENSORFS_TEST_PARENT:-/var/tmp}
test_parent=$(cd -- "$test_parent" && pwd -P)
test_root=$(mktemp -d "$test_parent/censorfs-smoke.XXXXXX")
daemon_pid=
cleanup() {
  if [[ -n "$daemon_pid" ]]; then kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" 2>/dev/null || true; fi
  if [[ $(dirname -- "$test_root") == "$test_parent" && $(basename -- "$test_root") == censorfs-smoke.* ]]; then
    rm -rf -- "$test_root"
  fi
}
trap cleanup EXIT

backing=$(stat -f -c %T "$test_root")
if [[ "$backing" != xfs && "$backing" != ext2/ext3 ]]; then
  echo "smoke root must be local XFS or ext4; found $backing" >&2
  exit 2
fi

mkdir "$test_root/import"
printf seed >"$test_root/import/seed.txt"
store="$test_root/.censorfs"
socket="$test_root/control.sock"
"$bin/censorfsctl" init --storage-root "$store" --import-root "$test_root/import" --branch main >/dev/null
"$bin/censorfsd" --storage-root "$store" --socket "$socket" &
daemon_pid=$!
for _ in $(seq 1 100); do [[ -S "$socket" ]] && break; sleep 0.05; done
[[ -S "$socket" ]]

head_json=$("$bin/censorfsctl" --socket "$socket" branch-head main)
base_generation=$(jq -r .generation_id <<<"$head_json")
base_seq=$(jq -r .head_seq <<<"$head_json")
tx_id=$("$bin/censorfsctl" --socket "$socket" begin-tx | jq -r .tx_id)
ticket_json=$("$bin/censorfsctl" --socket "$socket" begin-ticket "$tx_id" main)
ticket_id=$(jq -r .ticket_id <<<"$ticket_json")
view_id=$("$bin/censorfsctl" --socket "$socket" open-ticket "$ticket_id" | jq -r .view_id)

"$bin/censorfs-mounter" --socket "$socket" --view-id "$view_id" --uid 0 --gid 0 -- \
  /bin/sh -c 'test "$(cat /workspace/seed.txt)" = seed; printf changed >/workspace/seed.txt'

candidate_json=$("$bin/censorfsctl" --socket "$socket" prepare "$ticket_id")
candidate_id=$(jq -r .candidate.candidate_id <<<"$candidate_json")
"$bin/censorfsctl" --socket "$socket" publish "$candidate_id" "$base_generation" "$base_seq" smoke >/dev/null

stable_view=$("$bin/censorfsctl" --socket "$socket" open-branch main | jq -r .view_id)
"$bin/censorfs-mounter" --socket "$socket" --view-id "$stable_view" --uid 0 --gid 0 --read-only -- \
  /bin/sh -c 'test "$(cat /workspace/seed.txt)" = changed'

echo "real FUSE and mount-namespace smoke test passed"
