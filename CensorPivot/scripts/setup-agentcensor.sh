#!/usr/bin/env bash
# Host deployment only: never replace a live cgroup v1 mount or reboot the host.
set -euo pipefail

setup_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
install_deps=false
configure_boot=false
dsh_root=
run_demo=false
build_only=false

usage() {
  cat <<'EOF'
Usage: sudo -H bash CensorPivot/scripts/setup-agentcensor.sh ACTION [OPTIONS]

Actions:
  prepare  Load FUSE, inspect kernel/cgroup requirements; optionally prepare boot
  check    Check build and runtime prerequisites (--build-only skips runtime)
  install  Build/install FS/Guard/Scope/Pivot without starting services
  start    Check runtime, start installed services and run doctor (no rebuilding)
  run      Check, stop unified services, install FS/Guard/Scope/Pivot, start, doctor

Options:
  --install-deps    prepare/install: install native build packages using dnf/apt-get
  --configure-boot  prepare: enable cgroup v2/BPF LSM for the default grubby kernel
  --build-only      check: check build tools/BTF without requiring runtime readiness
  --dsh-root PATH   install/start/run: install DSH profiles for the invoking sudo user
  --demo           start/run: execute the real batch demo (publishes a fixture to main)

prepare exits 1 when boot configuration is missing, or 10 after a successful
boot update awaiting reboot. It never reboots automatically.
Existing component configs are preserved. DSH Web is started separately.
EOF
}

info() { printf '[OK] %s\n' "$*"; }
error() { printf '[ERROR] %s\n' "$*" >&2; }
fail() { error "$*"; exit 1; }
require_root() { [[ $(id -u) == 0 ]] || fail 'run this script with sudo'; }

is_cgroup_v2() {
  [[ -f /sys/fs/cgroup/cgroup.controllers ]] &&
    [[ $(stat -fc %T /sys/fs/cgroup) == cgroup2fs ]]
}

has_bpf_lsm() {
  [[ -r /sys/kernel/security/lsm ]] &&
    [[ ,$(cat /sys/kernel/security/lsm), == *,bpf,* ]]
}

boot_args_ready() {
  python3 -c '
import re, shlex, sys
raw = sys.stdin.read()
m = re.search(r"^args=\"(.*)\"$", raw, re.M)
tokens = [a.split("=", 1) for a in shlex.split(m[1] if m else raw) if "=" in a]
args = dict(tokens)
required = {"systemd.unified_cgroup_hierarchy", "cgroup_no_v1", "lsm"}
unique = all(sum(key == wanted for key, _ in tokens) == 1 for wanted in required)
ready = (unique and args.get("systemd.unified_cgroup_hierarchy") == "1"
         and args.get("cgroup_no_v1") == "all"
         and "bpf" in args.get("lsm", "").split(","))
if len(sys.argv) > 1 and sys.argv[1]:
    ready = ready and args.get("lsm") == sys.argv[1]
raise SystemExit(0 if ready else 1)
' "${1:-}"
}

kernel_config() {
  if [[ -r /proc/config.gz ]]; then
    gzip -dc /proc/config.gz
  elif [[ -r /boot/config-$(uname -r) ]]; then
    cat "/boot/config-$(uname -r)"
  else
    return 1
  fi
}

check_kernel() {
  local mode=${1:-runtime}
  local config
  if [[ $mode == build ]]; then
    [[ -r /sys/kernel/btf/vmlinux ]] || { error 'kernel BTF /sys/kernel/btf/vmlinux missing (required to build Guard)'; return 1; }
    info 'kernel BTF available for building Guard'
    return
  fi
  if config=$(kernel_config); then
    local flag
    for flag in CONFIG_CGROUPS CONFIG_BPF CONFIG_BPF_SYSCALL CONFIG_BPF_LSM CONFIG_DEBUG_INFO_BTF; do
      [[ $'\n'$config$'\n' == *$'\n'"$flag=y"$'\n'* ]] || {
        error "$flag is disabled; install a kernel supporting it before continuing"
        return 1
      }
    done
    [[ $'\n'$config$'\n' == *$'\nCONFIG_FUSE_FS=y\n'* ||
       $'\n'$config$'\n' == *$'\nCONFIG_FUSE_FS=m\n'* ]] || {
      error 'CONFIG_FUSE_FS is disabled; install a kernel supporting FUSE'
      return 1
    }
  else
    printf '[WARN] Kernel config unavailable; checking BTF, FUSE and active LSM directly\n'
  fi
  [[ -r /sys/kernel/btf/vmlinux ]] || { error 'kernel BTF /sys/kernel/btf/vmlinux missing'; return 1; }
  info 'kernel supports BTF, BPF LSM, cgroups and FUSE'
}

install_packages() {
  if command -v dnf >/dev/null; then
    dnf install -y gcc gcc-c++ make clang llvm bpftool libbpf-devel elfutils-libelf-devel \
      zlib-devel pkgconf-pkg-config fuse3 fuse3-devel python3 sudo \
      util-linux kmod gzip git curl ca-certificates
  elif command -v apt-get >/dev/null; then
    apt-get update
    local bpf_package=bpftool
    if ! apt-cache show bpftool >/dev/null 2>&1; then
      bpf_package="linux-tools-$(uname -r)"
    fi
    DEBIAN_FRONTEND=noninteractive apt-get install -y build-essential clang llvm \
      "$bpf_package" libbpf-dev libelf-dev zlib1g-dev pkg-config \
      fuse3 libfuse3-dev python3 sudo util-linux kmod gzip git curl ca-certificates
  else
    fail 'unsupported package manager; install dependencies listed in INSTALL.zh-CN.md manually'
  fi
  info 'native build packages installed; Rust and Node/pnpm are checked separately'
}

# Preserve all active/default-kernel LSMs when adding bpf. Validate before passing
# the result to the bootloader, rather than replacing SELinux/AppArmor settings.
lsm_with_bpf() {
  local list=$1
  [[ $list =~ ^[a-zA-Z0-9_,-]+$ ]] || return 1
  if [[ ,$list, == *,bpf,* ]]; then printf '%s\n' "$list";
  else printf '%s,bpf\n' "$list"; fi
}

prepare_boot() {
  command -v grubby >/dev/null || fail \
    '--configure-boot requires grubby (openEuler/RHEL); use the manual GRUB steps in INSTALL.zh-CN.md'
  command -v python3 >/dev/null || fail 'python3 is required to inspect default kernel arguments'
  local default_info lsm args backup path default_kernel updated_info
  default_info=$(grubby --info=DEFAULT) || fail 'grubby could not read the default kernel; no boot settings were changed'
  [[ -n $default_info ]] || fail 'grubby could not identify the default kernel'
  default_kernel=$(sed -n 's/^kernel=//p' <<< "$default_info")
  default_kernel=${default_kernel#\"}
  default_kernel=${default_kernel%\"}
  info "running kernel: $(uname -r); default boot kernel: ${default_kernel:-see grubby output}"
  # A failed reboot/booting another entry must not produce another backup and
  # repeatedly rewrite the same plan. Report the pending plan instead.
  if boot_args_ready <<< "$default_info"; then
    info 'default boot kernel already configured; no boot files changed'
    printf '%s\n' "$default_info"
    return
  fi
  lsm=$(python3 -c '
import re, shlex, sys
m = re.search(r"^args=\"(.*)\"$", sys.stdin.read(), re.M)
if not m: raise SystemExit("cannot parse grubby default kernel args")
print(next((a[4:] for a in shlex.split(m[1]) if a.startswith("lsm=")), ""))
' <<< "$default_info")
  if [[ -z $lsm && -r /sys/kernel/security/lsm ]]; then lsm=$(cat /sys/kernel/security/lsm); fi
  if [[ -z $lsm ]]; then
    lsm=$(kernel_config | sed -n 's/^CONFIG_LSM="\([^"]*\)"$/\1/p')
  fi
  [[ -n $lsm ]] || fail 'cannot determine existing LSM list; configure lsm= manually'
  lsm=$(lsm_with_bpf "$lsm") || fail 'invalid existing LSM list'
  args="systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=$lsm"
  backup=$(mktemp -d /var/lib/agentcensor-boot-backup.XXXXXX)
  printf '%s\n' "$default_info" > "$backup/default-kernel.txt"
  printf '%s\n' "$args" > "$backup/agentcensor-args.txt"
  for path in /etc/default/grub /boot/loader/entries /boot/grub2/grub.cfg /boot/grub/grub.cfg; do
    [[ ! -e $path ]] || cp -a --parents -- "$path" "$backup/"
  done
  info "default kernel boot plan: $args"
  # --args replaces existing values. On legacy grubby 8.40, combining it with
  # --remove-args for the same keys silently removes the newly added values.
  grubby --update-kernel=DEFAULT --args="$args" ||
    fail "grubby could not apply the boot plan; review $backup before retrying"
  updated_info=$(grubby --info=DEFAULT) ||
    fail "cannot verify the updated boot entry; do not reboot yet; review $backup"
  printf '%s\n' "$updated_info"
  if ! boot_args_ready "$lsm" <<< "$updated_info"; then
    fail "boot update did not persist the required arguments; do not reboot yet; original configuration is saved in $backup"
  fi
  info "default kernel boot arguments verified; original boot configuration saved in $backup"
}

prepare() {
  require_root
  [[ $install_deps == false ]] || install_packages
  [[ $(uname -s) == Linux ]] || fail 'Linux is required'
  case $(uname -m) in x86_64|aarch64) ;; *) fail 'supported architectures: x86_64, aarch64';; esac
  check_kernel || fail 'kernel requirements are not satisfied'
  ensure_fuse
  ensure_securityfs
  if is_cgroup_v2 && has_bpf_lsm; then
    info 'cgroup v2 and BPF LSM already active; proceed with run'
    return
  fi
  is_cgroup_v2 || error 'runtime requirement missing: cgroup v2 at /sys/fs/cgroup (needed by Pivot)'
  has_bpf_lsm || error 'runtime requirement missing: active BPF LSM (needed by Guard)'
  if [[ $configure_boot == true ]]; then
    if systemd-detect-virt --container --quiet; then
      fail 'container boot settings belong to the host; use a host/VM with delegated cgroup v2'
    fi
    if boot_args_ready < /proc/cmdline; then
      fail 'boot parameters are already active but runtime checks failed; inspect cgroup mounts and systemd/kernel support instead of rebooting again'
    fi
    prepare_boot
  else
    error 'boot settings were not changed; rebooting alone will not fix these requirements'
    printf '[NEXT] Run prepare --configure-boot, or follow the manual GRUB steps.\n' >&2
    printf '[NEXT] You can build/install first with: sudo -H bash %s/setup-agentcensor.sh install\n' "$setup_dir" >&2
    return 1
  fi
  printf '[REBOOT REQUIRED] Boot settings are prepared. Reboot once during a maintenance window, then run start (or run for a fresh install).\n'
  printf '[VERIFY] After reboot, compare /proc/cmdline and uname -r with the default kernel plan above; do not repeat reboots if the plan was not applied.\n'
  return 10
}

ensure_fuse() {
  if [[ ! -c /dev/fuse ]]; then modprobe fuse; fi
  [[ -c /dev/fuse ]] || fail '/dev/fuse is unavailable'
}

ensure_securityfs() {
  if [[ ! -r /sys/kernel/security/lsm ]]; then
    mkdir -p /sys/kernel/security
    mountpoint -q /sys/kernel/security || mount -t securityfs securityfs /sys/kernel/security
  fi
}

check_storage() {
  local storage=/var/lib/censorfs/.censorfs existing fs_type
  if [[ -r /etc/agentcensor/censord.json ]]; then
    storage=$(python3 -c 'import json; print(json.load(open("/etc/agentcensor/censord.json"))["censorfs"]["storage_root"])') || return 1
  fi
  existing=$storage
  while [[ ! -e $existing && $existing != / ]]; do existing=$(dirname -- "$existing"); done
  fs_type=$(findmnt -T "$existing" -n -o FSTYPE) || return 1
  case $fs_type in ext4|xfs) info "FS storage: $storage ($fs_type)";;
    *) error "FS storage is on $fs_type; configure local ext4/XFS before deployment"; return 1;; esac
}

check_cgroup() {
  is_cgroup_v2 || {
    error 'cgroup v2 is not mounted at /sys/fs/cgroup; run prepare --configure-boot before rebooting'
    printf '[CHECK] If already rebooted, compare cat /proc/cmdline, uname -r and sudo grubby --info=DEFAULT.\n' >&2
    return 1
  }
  local probe
  probe=$(mktemp -d /sys/fs/cgroup/agentcensor-preflight.XXXXXX) || return 1
  local result=0
  [[ -f $probe/cgroup.kill && -w $probe/cgroup.procs && -w $probe/cgroup.kill ]] || result=1
  rmdir -- "$probe" || result=1
  [[ $result == 0 ]] || { error 'cgroup v2 is not writable or cgroup.kill is unavailable'; return 1; }
  info "writable cgroup v2; controllers: $(cat /sys/fs/cgroup/cgroup.controllers)"
}

check_environment() {
  require_root
  local mode=${1:-full}
  local errors=0 command version
  [[ $(uname -s) == Linux ]] || fail 'Linux is required'
  case $(uname -m) in x86_64|aarch64) ;; *) fail 'supported architectures: x86_64, aarch64';; esac
  local commands=(systemctl python3 sudo runuser findmnt gzip)
  if [[ $mode != runtime ]]; then
    commands+=(gcc make clang llvm-strip bpftool pkg-config visudo useradd groupadd)
  fi
  if [[ $mode != build ]]; then commands+=(systemd-detect-virt mountpoint modprobe); fi
  for command in "${commands[@]}"; do
    if ! command -v "$command" >/dev/null; then error "missing command: $command"; errors=$((errors + 1)); fi
  done
  if [[ $mode == build ]]; then
    check_kernel build || errors=$((errors + 1))
  else
    if [[ ! -d /run/systemd/system ]]; then error 'systemd must be the running service manager'; errors=$((errors + 1)); fi
    check_kernel || errors=$((errors + 1))
    if [[ ! -c /dev/fuse ]]; then error 'FUSE missing; run prepare or modprobe fuse'; errors=$((errors + 1)); fi
    if ! has_bpf_lsm; then error 'BPF LSM inactive; run prepare --configure-boot before rebooting'; errors=$((errors + 1)); fi
    check_cgroup || errors=$((errors + 1))
    if command -v python3 >/dev/null && command -v findmnt >/dev/null; then check_storage || errors=$((errors + 1)); fi
  fi
  if [[ $mode != runtime ]] && command -v clang >/dev/null; then
    version=$(clang --version | sed -n 's/.*clang version \([0-9]*\).*/\1/p' | head -n 1)
    if [[ ! $version =~ ^[0-9]+$ ]] || (( version < 17 )); then error 'clang 17+ required'; errors=$((errors + 1)); fi
    if ! printf 'int probe;\n' | clang -target bpf -x c -c -o /dev/null -; then error 'clang BPF backend unavailable'; errors=$((errors + 1)); fi
  fi
  if [[ $mode != runtime ]] && command -v gcc >/dev/null; then
    if ! printf '#include <bpf/libbpf.h>\n#include <libelf.h>\n#include <zlib.h>\nint main(void) { return 0; }\n' |
      gcc -x c - -lbpf -lelf -lz -o /dev/null; then
      error 'libbpf/libelf/zlib development headers or libraries missing'; errors=$((errors + 1))
    fi
  fi
  if [[ $mode != runtime ]] && ! command -v cargo >/dev/null && [[ ! -x ${CARGO_HOME:-$HOME/.cargo}/bin/cargo ]]; then
    error 'Cargo missing; install Rust using the steps in INSTALL.zh-CN.md'; errors=$((errors + 1))
  fi
  if [[ $mode != runtime ]] && ! command -v rustup >/dev/null && [[ ! -x ${CARGO_HOME:-$HOME/.cargo}/bin/rustup ]]; then
    version=$(rustc --version 2>/dev/null || true)
    error "rustup required for pinned toolchains (current: ${version:-missing}); install rustup"; errors=$((errors + 1))
  fi
  [[ $errors == 0 ]] || { error "$errors environment checks failed; installation/start was not attempted"; return 1; }
  info "$mode environment checks passed"
  if [[ $mode == build ]]; then
    printf '[INFO] Runtime cgroup v2, active BPF LSM and FUSE were not required; install does not start services.\n'
  fi
}

as_dsh_user() {
  local user=${SUDO_USER:-root}
  # Restore the target user's login PATH even when root invoked sudo; its
  # secure_path commonly hides Node installed through nvm.
  if [[ $user == root ]]; then bash -lc 'exec "$@"' bash "$@";
  else sudo -H -u "$user" bash -lc 'exec "$@"' bash "$@"; fi
}

check_dsh_environment() {
  # Make rustup's cargo discoverable after sudo's secure_path filtering.
  if [[ -x ${CARGO_HOME:-$HOME/.cargo}/bin/cargo ]]; then export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"; fi
  if [[ -n $dsh_root ]]; then
    [[ -f $dsh_root/package.json ]] || fail "invalid DSH source directory: $dsh_root"
    as_dsh_user bash -c 'node -e "if (+process.versions.node.split(\".\")[0] < 22) process.exit(1)" && command -v pnpm >/dev/null' ||
      fail 'DSH requires Node 22+ and pnpm in the invoking user environment'
    as_dsh_user bash -c 'set -euo pipefail; cd -- "$1"; pnpm dsh --version | grep -Fx "0.1.5-rc.2" >/dev/null' bash "$dsh_root" ||
      fail 'DSH source/dependencies must provide version 0.1.5-rc.2 before deployment'
  fi
}

stop_existing_services() {
  local service
  for service in censorpivot.service agentcensord.service; do
    if systemctl cat "$service" >/dev/null 2>&1; then systemctl stop "$service"; fi
  done
  for service in censorfsd.service censorguardd.service; do
    if systemctl is-active --quiet "$service"; then
      fail "standalone $service is running; stop/disable it before unified deployment"
    fi
  done
  if [[ -x /usr/sbin/censorscoped ]] && /usr/sbin/censorscoped status 2>/dev/null | grep -q ' running pid='; then
    fail 'standalone CensorScope is running; stop it before unified deployment'
  fi
}

install_dsh_profiles() {
  if [[ -n $dsh_root ]]; then
    as_dsh_user env DSH_ROOT="$dsh_root" bash "$setup_dir/install-dsh-censorpivot.sh"
    printf '[NEXT] Once native services are healthy, start Web as %s from %s: pnpm dsh --profile web\n' "${SUDO_USER:-root}" "$dsh_root"
    printf '[NEXT] Ensure ~/.local/bin is on that user\x27s PATH.\n'
  fi
}

install_stack() {
  require_root
  [[ $install_deps == false ]] || install_packages
  check_environment build || return 1
  check_dsh_environment
  stop_existing_services
  bash "$setup_dir/install-agentcensor.sh" all
  install_dsh_profiles
  info 'FS, Guard, Scope and Pivot files installed; services were not started'
  printf '[NEXT] When runtime prerequisites are ready: sudo -H bash %s/setup-agentcensor.sh start\n' "$setup_dir"
}

start_installed_stack() {
  bash "$setup_dir/install-agentcensor.sh" start
  bash "$setup_dir/install-agentcensor.sh" doctor
  if [[ $run_demo == true ]]; then
    CENSORPIVOT_DEMO_GUARD_GROUP=censorguard-dsh-default bash "$setup_dir/run-atomic-code-change-demo.sh"
  fi
  install_dsh_profiles
  info 'FS, Guard, Scope and Pivot started and healthy'
}

start_stack() {
  require_root
  ensure_fuse
  ensure_securityfs
  check_environment runtime || return 1
  check_dsh_environment
  start_installed_stack
}

run_stack() {
  require_root
  ensure_fuse
  ensure_securityfs
  check_environment || return 1
  check_dsh_environment
  stop_existing_services
  bash "$setup_dir/install-agentcensor.sh" all
  start_installed_stack
}

main() {
  local action=${1:-help}
  [[ $# == 0 ]] || shift
  while (( $# )); do
    case $1 in
      --install-deps) install_deps=true; shift;;
      --configure-boot) configure_boot=true; shift;;
      --build-only) build_only=true; shift;;
      --dsh-root) [[ $# -ge 2 ]] || fail '--dsh-root requires a path'; dsh_root=$(realpath -- "$2"); shift 2;;
      --demo) run_demo=true; shift;;
      *) fail "unknown option: $1";;
    esac
  done
  case $action in
    prepare)
      [[ -z $dsh_root && $run_demo == false && $build_only == false ]] || fail 'prepare does not accept DSH/demo/build-only options'
      prepare;;
    install)
      [[ $configure_boot == false && $run_demo == false && $build_only == false ]] || fail 'install does not accept boot/demo/build-only options'
      install_stack;;
    check|start|run)
      [[ $install_deps == false && $configure_boot == false ]] || fail 'package options require prepare/install; boot options require prepare'
      if [[ $action == check ]]; then
        [[ -z $dsh_root && $run_demo == false ]] || fail 'DSH/demo options require install/start/run'
        if [[ $build_only == true ]]; then check_environment build; else check_environment; fi
      else
        [[ $build_only == false ]] || fail '--build-only requires check'
        if [[ $action == start ]]; then start_stack; else run_stack; fi
      fi;;
    help|-h|--help) usage;;
    *) usage >&2; fail "unknown action: $action";;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
