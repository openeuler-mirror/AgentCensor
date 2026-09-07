#!/usr/bin/env bash
set -euo pipefail

demo_parent=${CENSORFS_DEMO_PARENT:-/var/tmp}
demo_root=${CENSORFS_DEMO_ROOT:-$(mktemp -d "$demo_parent/censorfs-merge.XXXXXX")}
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
  for _ in $(seq 1 100); do [[ -S "$CENSORFS_SOCKET" ]] && return; sleep 0.05; done
  exit 2
}

step "初始化并创建 source/target 分支"
mkdir -p "$demo_root/import"
printf 'base\n' >"$demo_root/import/base.txt"
censorfs-init --import-root "$demo_root/import" --branch main
censorfs-daemon >"$demo_root/daemon.log" 2>&1 & daemon_pid=$!
wait_daemon
censorfs-branch-create source --from main
censorfs-branch-create target --from main

step "两侧修改不同路径并分别提交"
source_ticket=$(censorfs-explore source --id-only)
target_ticket=$(censorfs-explore target --id-only)
censorfs-write --ticket "$source_ticket" /from-source.txt --text $'source\n'
censorfs-write --ticket "$target_ticket" /from-target.txt --text $'target\n'
censorfs-commit "$source_ticket" --message "source change"
censorfs-commit "$target_ticket" --message "target change"

step "--check 只检查，不移动 Target Head"
target_before_check=$(censorfs-head target --generation-only)
censorfs-merge source --into target --check
target_after_check=$(censorfs-head target --generation-only)
[[ "$target_before_check" == "$target_after_check" ]]

step "执行三方合并并验证双父 Generation"
merge_output=$(censorfs-merge source --into target --message "merge source into target")
printf '%s\n' "$merge_output"
merge_generation=$(sed -n 's/^merged-generation: //p' <<<"$merge_output")
[[ -n "$merge_generation" ]]
generation_info=$(censorfs-generation "$merge_generation")
printf '%s\n' "$generation_info"
grep -q '^kind: Merge$' <<<"$generation_info"
grep -q '^parents: .*,' <<<"$generation_info"
[[ $(censorfs-cat --branch target /from-source.txt) == source ]]
[[ $(censorfs-cat --branch target /from-target.txt) == target ]]

step "构造同一路径冲突，检查和实际合并都不得移动 Target Head"
censorfs-branch-create conflict-source --from target
censorfs-branch-create conflict-target --from target
left_ticket=$(censorfs-explore conflict-source --id-only)
right_ticket=$(censorfs-explore conflict-target --id-only)
censorfs-write --ticket "$left_ticket" /same.txt --text $'left\n'
censorfs-write --ticket "$right_ticket" /same.txt --text $'right\n'
censorfs-commit "$left_ticket" --message "left conflict"
censorfs-commit "$right_ticket" --message "right conflict"
conflict_head=$(censorfs-head conflict-target --generation-only)
if censorfs-merge conflict-source --into conflict-target --check; then
  echo "expected merge check conflict" >&2
  exit 1
fi
[[ $(censorfs-head conflict-target --generation-only) == "$conflict_head" ]]
if censorfs-merge conflict-source --into conflict-target; then
  echo "expected merge conflict" >&2
  exit 1
fi
[[ $(censorfs-head conflict-target --generation-only) == "$conflict_head" ]]

echo "PASS: 三方合并、双父 Generation、只检查模式和冲突不落盘均通过"
