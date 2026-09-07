#!/usr/bin/env bash
set -euo pipefail

demo_parent=${CENSORFS_DEMO_PARENT:-/var/tmp}
demo_root=${CENSORFS_DEMO_ROOT:-$(mktemp -d "$demo_parent/censorfs-isolation.XXXXXX")}
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
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  echo "保留的测试现场: $demo_root"
  echo "daemon 日志: $demo_root/daemon.log"
}
trap cleanup EXIT

step() { printf '\n==> %s\n' "$1"; }
wait_daemon() {
  for _ in $(seq 1 100); do
    [[ -S "$CENSORFS_SOCKET" ]] && return
    sleep 0.05
  done
  echo "daemon did not create $CENSORFS_SOCKET" >&2
  exit 2
}

step "初始化共同基础版本"
mkdir -p "$demo_root/import"
printf 'base\n' >"$demo_root/import/shared.txt"
censorfs-init --import-root "$demo_root/import" --branch main
censorfs-daemon >"$demo_root/daemon.log" 2>&1 &
daemon_pid=$!
wait_daemon

step "从同一个 main Generation 创建两个分支"
censorfs-branch-create branch-a --from main
censorfs-branch-create branch-b --from main
censorfs-branch-list

step "打开两个互相隔离的 Ticket"
ticket_a=$(censorfs-explore branch-a --id-only)
ticket_b=$(censorfs-explore branch-b --id-only)
echo "branch-a ticket: $ticket_a"
echo "branch-b ticket: $ticket_b"

step "两个 Ticket 写入不同内容"
censorfs-write --ticket "$ticket_a" /shared.txt --text $'content-a\n'
censorfs-mkdir --ticket "$ticket_a" /only-a
censorfs-write --ticket "$ticket_a" /only-a/file.txt --text $'a\n'
censorfs-write --ticket "$ticket_b" /shared.txt --text $'content-b\n'
censorfs-mkdir --ticket "$ticket_b" /only-b
censorfs-write --ticket "$ticket_b" /only-b/file.txt --text $'b\n'

step "验证私有修改没有串扰，稳定分支仍是 base"
[[ $(censorfs-cat --ticket "$ticket_a" /shared.txt) == content-a ]]
[[ $(censorfs-cat --ticket "$ticket_b" /shared.txt) == content-b ]]
[[ $(censorfs-cat --branch branch-a /shared.txt) == base ]]
[[ $(censorfs-cat --branch branch-b /shared.txt) == base ]]
censorfs-ls --ticket "$ticket_a" /
censorfs-ls --ticket "$ticket_b" /

step "分别提交，得到彼此独立的 Generation 和 Head"
generation_a=$(censorfs-commit "$ticket_a" --message "branch-a exploration" --generation-only)
generation_b=$(censorfs-commit "$ticket_b" --message "branch-b exploration" --generation-only)
[[ "$generation_a" != "$generation_b" ]]
[[ $(censorfs-cat --branch branch-a /shared.txt) == content-a ]]
[[ $(censorfs-cat --branch branch-b /shared.txt) == content-b ]]
censorfs-head branch-a
censorfs-head branch-b
censorfs-diff "$generation_a" "$generation_b"

echo "PASS: 双分支 Ticket、目录内容、Head 和 Generation 均相互隔离"
