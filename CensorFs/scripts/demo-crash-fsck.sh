#!/usr/bin/env bash
set -euo pipefail

demo_parent=${CENSORFS_DEMO_PARENT:-/var/tmp}
demo_root=${CENSORFS_DEMO_ROOT:-$(mktemp -d "$demo_parent/censorfs-crash.XXXXXX")}
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

printf '\n==> 创建 OPEN Ticket、写入私有数据，然后强杀 daemon\n'
head_before=$(censorfs-head main --generation-only)
ticket=$(censorfs-explore main --id-only)
censorfs-write --ticket "$ticket" /value.txt --text $'unpublished\n'
kill -9 "$daemon_pid"
wait "$daemon_pid" 2>/dev/null || true
daemon_pid=

printf '\n==> 纯检查必须报告待恢复的 OPEN Ticket\n'
if censorfs-fsck; then
  echo "fsck unexpectedly reported clean" >&2
  exit 1
fi

printf '\n==> 显式修复会 Abort 无 Candidate 的 OPEN Ticket，并自动复检\n'
censorfs-fsck --repair

printf '\n==> 重启 daemon，验证稳定 Head 和内容未变化\n'
rm -f -- "$CENSORFS_SOCKET"
censorfs-daemon >>"$demo_root/daemon.log" 2>&1 & daemon_pid=$!
wait_daemon
[[ $(censorfs-head main --generation-only) == "$head_before" ]]
[[ $(censorfs-cat --branch main /value.txt) == stable ]]
if censorfs-write --ticket "$ticket" /late.txt --text late; then
  echo "recovered-aborted ticket unexpectedly accepted a write" >&2
  exit 1
fi

echo "PASS: kill -9 后 fsck 检出并安全修复 OPEN Ticket，稳定数据未改变"
