#!/usr/bin/env bash
set -euo pipefail

demo_parent=${CENSORFS_DEMO_PARENT:-/var/tmp}
demo_root=${CENSORFS_DEMO_ROOT:-$(mktemp -d "$demo_parent/censorfs-abort.XXXXXX")}
mkdir -p "$demo_root"
demo_backing=$(stat -f -c %T "$demo_root")
if [[ "$demo_backing" != xfs && "$demo_backing" != ext2/ext3 ]]; then
  echo "demo root must be on local XFS/ext4; found $demo_backing" >&2
  exit 2
fi
export CENSORFS_STORAGE_ROOT="$demo_root/.censorfs"
export CENSORFS_SOCKET="$demo_root/control.sock"
daemon_pid=

cleanup() {
  if [[ -n "$daemon_pid" ]]; then kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" 2>/dev/null || true; fi
  echo "保留的测试现场: $demo_root"
  echo "daemon 日志: $demo_root/daemon.log"
}
trap cleanup EXIT
wait_daemon() { for _ in $(seq 1 100); do [[ -S "$CENSORFS_SOCKET" ]] && return; sleep 0.05; done; exit 2; }

mkdir -p "$demo_root/import"
printf 'stable\n' >"$demo_root/import/value.txt"
censorfs-init --import-root "$demo_root/import" --branch main
censorfs-daemon >"$demo_root/daemon.log" 2>&1 & daemon_pid=$!
wait_daemon

printf '\n==> 打开单分支探索并查看私有修改\n'
head_before=$(censorfs-head main --generation-only)
ticket=$(censorfs-explore main --id-only)
censorfs-write --ticket "$ticket" /value.txt --text $'private\n'
censorfs-mkdir --ticket "$ticket" /scratch
censorfs-write --ticket "$ticket" /scratch/note.txt --text $'temporary\n'
censorfs-ls --ticket "$ticket" /
[[ $(censorfs-cat --ticket "$ticket" /value.txt) == private ]]

printf '\n==> Abort 后稳定内容和 Head 必须完全不变\n'
censorfs-abort "$ticket"
[[ $(censorfs-head main --generation-only) == "$head_before" ]]
[[ $(censorfs-cat --branch main /value.txt) == stable ]]

printf '\n==> 已 Abort Ticket 不能继续写入或提交\n'
if censorfs-write --ticket "$ticket" /late.txt --text late; then
  echo "write unexpectedly succeeded after abort" >&2
  exit 1
fi
if censorfs-commit "$ticket"; then
  echo "commit unexpectedly succeeded after abort" >&2
  exit 1
fi

echo "PASS: Abort 丢弃私有 Upper，稳定分支和 Head 不变"
