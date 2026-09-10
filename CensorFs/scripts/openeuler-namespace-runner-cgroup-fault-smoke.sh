#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Linux || $(id -u) -ne 0 ]]; then
  echo 'cgroup fault smoke requires Linux root' >&2
  exit 2
fi
command -v jq >/dev/null
command -v node >/dev/null
command -v python3 >/dev/null
command -v setsid >/dev/null
command -v mount >/dev/null
command -v umount >/dev/null
[[ -c /dev/fuse ]]

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${CENSORFS_BIN_DIR:-"$repo_root/target/release"}
for program in censorfsd censorfsctl censorfs-mounter; do [[ -x "$bin/$program" ]]; done

uid=${CENSORFS_AGENT_UID:-65534}
gid=${CENSORFS_AGENT_GID:-65534}
[[ $uid -ne 0 && $gid -ne 0 ]]
agent=(setpriv --reuid "$uid" --regid "$gid" --clear-groups)
test_parent=${CENSORFS_TEST_PARENT:-/var/tmp}
test_root=$(mktemp -d "$test_parent/censorfs-cgroup-fault.XXXXXX")
cgroup_mount=/sys/fs/cgroup
owns_cgroup_mount=0
if [[ $(stat -f -c %T "$cgroup_mount") != cgroup2fs ]]; then
  cgroup_mount="$test_root/cgroup2"
  mkdir "$cgroup_mount"
  mount -t cgroup2 none "$cgroup_mount"
  owns_cgroup_mount=1
fi
cgroup_root="$cgroup_mount/censorfs-fault-$$"
state_dir="$test_root/cgroup-state"
mkdir "$cgroup_root"
chown "$uid:$gid" "$test_root"

cgroup_limit_args=()
controllers=$(cat "$cgroup_root/cgroup.controllers")
if grep -qw memory <<<"$controllers"; then cgroup_limit_args+=(--cgroup-memory-max 268435456); fi
if grep -qw pids <<<"$controllers"; then cgroup_limit_args+=(--cgroup-pids-max 64); fi
if grep -qw cpu <<<"$controllers"; then cgroup_limit_args+=(--cgroup-cpu-max '200000 100000'); fi

daemon_pid=
passed=0
cleanup() {
  if [[ -n "$daemon_pid" ]]; then kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" 2>/dev/null || true; fi
  if [[ -d "$cgroup_root" ]]; then
    find "$cgroup_root" -mindepth 1 -maxdepth 1 -type d -exec sh -c 'test ! -e "$1/cgroup.kill" || echo 1 >"$1/cgroup.kill"; rmdir "$1" 2>/dev/null || true' sh {} \;
    rmdir "$cgroup_root" 2>/dev/null || true
  fi
  if [[ $owns_cgroup_mount -eq 1 ]]; then umount "$cgroup_mount" 2>/dev/null || true; fi
  if [[ $passed -eq 1 ]]; then rm -rf "$test_root"; else echo "failed test data preserved at $test_root" >&2; fi
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

new_ticket() {
  local tx
  tx=$("${agent[@]}" "$bin/censorfsctl" --socket "$socket" begin-tx | jq -r .tx_id)
  "${agent[@]}" "$bin/censorfsctl" --socket "$socket" begin-ticket "$tx" main | jq -r .ticket_id
}

open_ticket() {
  "${agent[@]}" "$bin/censorfsctl" --socket "$socket" open-ticket "$1" | jq -r .view_id
}

run_scope() {
  local scope=$1 view=$2; shift 2
  "$bin/censorfs-mounter" \
    --socket "$socket" --view-id "$view" --uid "$uid" --gid "$gid" \
    --cgroup-root "$cgroup_root" --cgroup-state-dir "$state_dir" --cgroup-scope "$scope" \
    "${cgroup_limit_args[@]}" \
    --cgroup-cleanup-timeout-ms 5000 -- "$@"
}

# Normal exit always removes its leaf scope.
ticket=$(new_ticket)
view=$(open_ticket "$ticket")
run_scope runner-normal "$view" /bin/true
[[ ! -e "$cgroup_root/runner-normal" ]]

# A process that escapes the Runner process group with setsid remains in the cgroup and is killed.
ticket=$(new_ticket)
view=$(open_ticket "$ticket")
run_scope runner-setsid "$view" /bin/sh -c 'setsid sh -c "sleep 1; touch /workspace/setsid-leak" >/dev/null 2>&1 & exit 0'
sleep 1.3
sleep 0.1
view=$(open_ticket "$ticket")
run_scope runner-check-setsid "$view" /bin/sh -c 'test ! -e /workspace/setsid-leak'
[[ ! -e "$cgroup_root/runner-setsid" ]]

# A double-forked daemon is also inherited by the cgroup and cannot perform its delayed write.
ticket=$(new_ticket)
view=$(open_ticket "$ticket")
run_scope runner-double-fork "$view" python3 -c 'import os,time; p=os.fork();
if p==0:
 os.setsid(); p2=os.fork();
 if p2==0:
  time.sleep(1); open("/workspace/double-fork-leak","w").write("leak")
os._exit(0)'
sleep 1.3
sleep 0.1
view=$(open_ticket "$ticket")
run_scope runner-check-double "$view" /bin/sh -c 'test ! -e /workspace/double-fork-leak'
[[ ! -e "$cgroup_root/runner-double-fork" ]]

# Host-parent death triggers PR_SET_PDEATHSIG in the supervisor and cgroup-wide cleanup.
ticket=$(new_ticket)
view=$(open_ticket "$ticket")
cat >"$test_root/host-parent.py" <<PY
import subprocess,time
child=subprocess.Popen([
  '$bin/censorfs-mounter','--socket','$socket','--view-id','$view','--uid','$uid','--gid','$gid',
  '--cgroup-root','$cgroup_root','--cgroup-state-dir','$state_dir','--cgroup-scope','runner-host-death','--',
  '/bin/sh','-c','setsid sh -c "sleep 2; touch /workspace/host-death-leak" & sleep 30'])
open('$test_root/supervisor.pid','w').write(str(child.pid))
time.sleep(30)
PY
python3 "$test_root/host-parent.py" & host_parent=$!
for _ in $(seq 1 100); do [[ -s "$test_root/supervisor.pid" && -d "$cgroup_root/runner-host-death" ]] && break; sleep 0.05; done
kill -9 "$host_parent"
wait "$host_parent" 2>/dev/null || true
for _ in $(seq 1 100); do [[ ! -e "$cgroup_root/runner-host-death" ]] && break; sleep 0.05; done
[[ ! -e "$cgroup_root/runner-host-death" ]]
sleep 2.2
sleep 0.1
view=$(open_ticket "$ticket")
run_scope runner-check-host "$view" /bin/sh -c 'test ! -e /workspace/host-death-leak'

# SIGKILLing the supervisor leaves an owned stale leaf; the next launch recovers it.
ticket=$(new_ticket)
view=$(open_ticket "$ticket")
run_scope runner-stale "$view" /bin/sh -c 'setsid sleep 30 & sleep 30' & stale_supervisor=$!
for _ in $(seq 1 100); do [[ -d "$cgroup_root/runner-stale" ]] && break; sleep 0.05; done
[[ -d "$cgroup_root/runner-stale" ]]
kill -9 "$stale_supervisor"
wait "$stale_supervisor" 2>/dev/null || true
[[ -d "$cgroup_root/runner-stale" ]]
ticket=$(new_ticket)
view=$(open_ticket "$ticket")
run_scope runner-recovery "$view" /bin/true
[[ ! -e "$cgroup_root/runner-stale" ]]
[[ ! -e "$cgroup_root/runner-recovery" ]]

passed=1
echo 'cgroup v2 Runner scopes killed setsid/double-fork workloads, cleaned Host death, and recovered a stale scope'
