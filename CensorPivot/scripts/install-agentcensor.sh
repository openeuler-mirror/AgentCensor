#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
pivot_root=$(cd "$script_dir/.." && pwd)
repo_root=$(cd "$pivot_root/.." && pwd)
fs_root=${CENSORFS_ROOT:-$repo_root/CensorFs}
guard_root=${CENSORGUARD_ROOT:-$repo_root/CensorGuard}
scope_root=${CENSORSCOPE_ROOT:-$repo_root/CensorScope}
force_config=false

usage() {
  cat <<'EOF'
Usage: sudo CensorPivot/scripts/install-agentcensor.sh [--force-config] COMMAND

Commands:
  fs       Build and install CensorFS
  guard    Build and install CensorGuard and its AgentCensor policy
  scope    Build and install CensorScope
  pivot    Build and install CensorPivot, its service user, and systemd units
  all      Install fs, guard, scope, and pivot in that order
  start    Start the unified supervisor and CensorPivot services
  stop     Stop CensorPivot and the unified supervisor
  status   Show service and component health
  doctor   Check installation and runtime prerequisites with repair hints
  help     Show this help

Existing configuration is preserved. Use --force-config to replace it with
the repository examples (the previous file is saved with a .bak suffix).
EOF
}

info() { printf '[INFO] %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*" >&2; }
fail() {
  printf '[ERROR] %s\n' "$1" >&2
  [[ $# -lt 2 ]] || printf '[HINT] %s\n' "$2" >&2
  exit 1
}

require_root() {
  [[ $(id -u) -eq 0 ]] || fail "this command must run as root" "rerun with sudo"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1" "$2"
}

run_step() {
  local description=$1
  shift
  info "$description"
  "$@" || fail "$description failed" "review the command output above and fix the first reported error"
}

ensure_group() {
  if ! getent group censorpivot >/dev/null 2>&1; then
    run_step "creating system group censorpivot" groupadd --system censorpivot
  fi
}

ensure_user() {
  ensure_group
  if ! getent passwd censorpivot >/dev/null 2>&1; then
    local nologin=/sbin/nologin
    [[ -x "$nologin" ]] || nologin=/usr/sbin/nologin
    run_step "creating system user censorpivot" \
      useradd --system --gid censorpivot --home-dir /var/lib/censorpivot \
      --shell "$nologin" --comment "CensorPivot service" censorpivot
  fi
}

install_config() {
  local source=$1 destination=$2 mode=$3 owner=$4 group=$5
  if [[ -e "$destination" && "$force_config" == false ]]; then
    info "preserving existing configuration $destination"
    return
  fi
  if [[ -e "$destination" ]]; then
    cp -a -- "$destination" "$destination.bak"
    warn "saved previous configuration as $destination.bak"
  fi
  install -m "$mode" -o "$owner" -g "$group" "$source" "$destination"
}

require_source_tree() {
  [[ -f "$1/Cargo.toml" ]] || fail "$2 source tree not found at $1" \
    "set $3 to the component source directory"
}

install_fs() {
  require_root
  require_command cargo "install Rust and Cargo, then retry"
  require_source_tree "$fs_root" CensorFS CENSORFS_ROOT
  ensure_group
  run_step "building CensorFS release binaries" \
    cargo build --manifest-path "$fs_root/Cargo.toml" --workspace --release --locked
  for binary in censorfs censorfsd censorfsctl censorfs-mounter; do
    [[ -x "$fs_root/target/release/$binary" ]] || fail \
      "CensorFS build did not produce $binary" "rerun the build in $fs_root"
  done
  install -d -m 0755 /usr/local/bin /usr/libexec/censorfs
  install -m 0755 "$fs_root/target/release/censorfs" /usr/local/bin/censorfs
  install -m 0755 "$fs_root/target/release/censorfsd" /usr/libexec/censorfs/censorfsd
  install -m 0755 "$fs_root/target/release/censorfsctl" /usr/libexec/censorfs/censorfsctl
  install -m 0755 "$fs_root/target/release/censorfs-mounter" /usr/libexec/censorfs/censorfs-mounter
  install -d -m 0700 /var/lib/censorfs
  install -d -m 0755 /var/lib/agentcensor/workspace
  install -d -o root -g censorpivot -m 2770 /run/censorfs
  [[ -c /dev/fuse ]] || warn "/dev/fuse is missing; load the fuse module before start: sudo modprobe fuse"
  info "CensorFS installed"
}

install_guard() {
  require_root
  require_command cargo "install Rust and Cargo, then retry"
  require_command make "install make, then retry"
  require_command clang "install clang 17+ and the eBPF build dependencies"
  require_command llvm-strip "install llvm tools matching clang"
  require_command bpftool "install bpftool and libbpf development files"
  require_source_tree "$guard_root" CensorGuard CENSORGUARD_ROOT
  ensure_group
  [[ -r /sys/kernel/btf/vmlinux ]] || warn "/sys/kernel/btf/vmlinux is missing; CensorGuard cannot start on this kernel"
  if [[ ! -r /sys/kernel/security/lsm ]] || ! grep -Eq '(^|,)bpf(,|$)' /sys/kernel/security/lsm 2>/dev/null; then
    warn "BPF LSM is not enabled; add bpf to the kernel lsm= boot parameter and reboot before start"
  fi
  run_step "building the CensorGuard eBPF object" make -C "$guard_root" bpf
  run_step "building CensorGuard release binaries" \
    cargo build --manifest-path "$guard_root/Cargo.toml" --workspace --release --locked
  run_step "installing CensorGuard files" make -C "$guard_root" install-files
  if command -v systemd-sysusers >/dev/null 2>&1; then
    systemd-sysusers /usr/lib/sysusers.d/censorguard.conf
  elif ! getent group censorguard >/dev/null 2>&1; then
    groupadd --system censorguard
  fi
  install -d -m 0750 /etc/censorguard
  install_config "$pivot_root/deploy/censorguard/agentcensor.yaml" \
    /etc/censorguard/agentcensor.yaml 0640 root root
  install -d -m 0750 /etc/censorguard/policy.d
  install_config "$guard_root/config/policy.dsh-default.yaml" \
    /etc/censorguard/policy.d/censorguard-dsh-default.yaml 0640 root root
  install -d -m 0750 /var/lib/censorguard
  # Guard rejects a socket parent writable by group/other. Socket files are
  # chgrp'd separately through --socket-group.
  install -d -o root -g censorpivot -m 0750 /run/censorguard
  info "CensorGuard installed; the standalone censorguardd.service was not enabled"
}

install_scope() {
  require_root
  require_command cargo "install Rust 1.90+ and Cargo, then retry"
  require_source_tree "$scope_root" CensorScope CENSORSCOPE_ROOT
  ensure_group
  run_step "building CensorScope release binaries" \
    cargo build --manifest-path "$scope_root/Cargo.toml" --release --locked -p daemon -p ctl
  for binary in censorscoped censorscopectl; do
    [[ -x "$scope_root/target/release/$binary" ]] || fail \
      "CensorScope build did not produce $binary" "rerun the build in $scope_root"
  done
  install -d -m 0755 /usr/bin /usr/sbin
  install -m 0755 "$scope_root/target/release/censorscoped" /usr/sbin/censorscoped
  install -m 0755 "$scope_root/target/release/censorscopectl" /usr/bin/censorscopectl
  install -d -m 0755 /etc/censorscope /var/lib/censorscope /var/log/censorscope /run/censorscope
  if [[ ! -f /etc/censorscope/censorscoped.conf || "$force_config" == true ]]; then
    local init_args=()
    if [[ -f /etc/censorscope/censorscoped.conf ]]; then
      cp -a -- /etc/censorscope/censorscoped.conf /etc/censorscope/censorscoped.conf.bak
      init_args+=(--force)
    fi
    run_step "creating CensorScope operator configuration" \
      /usr/sbin/censorscoped --config /etc/censorscope/censorscoped.conf init "${init_args[@]}"
    chmod 0644 /etc/censorscope/censorscoped.conf
  else
    info "preserving existing configuration /etc/censorscope/censorscoped.conf"
    /usr/sbin/censorscoped --config /etc/censorscope/censorscoped.conf init >/dev/null || \
      fail "existing CensorScope configuration is invalid" \
        "fix it or rerun with --force-config after reviewing the backup"
  fi
  info "CensorScope installed"
}

require_component_file() {
  local component=$1 path=$2 command=$3
  [[ -x "$path" ]] || fail "$component is not installed: missing $path" \
    "run sudo $script_dir/install-agentcensor.sh $command"
}

install_pivot() {
  require_root
  require_command cargo "install Rust 1.88+ and Cargo, then retry"
  require_command sudo "install sudo; CensorPivot uses it for the privileged CensorFS mounter"
  require_command visudo "install the sudo package, then retry"
  require_command systemctl "CensorPivot service installation requires systemd"
  require_command runuser "install util-linux, then retry"
  require_component_file CensorFS /usr/libexec/censorfs/censorfsd fs
  require_component_file CensorGuard /usr/sbin/censorguardd guard
  require_component_file CensorScope /usr/sbin/censorscoped scope
  ensure_user
  run_step "building CensorPivot release binaries" \
    cargo build --manifest-path "$pivot_root/Cargo.toml" --release --locked
  install -d -m 0755 /usr/local/bin /usr/share/doc/censorpivot /usr/lib/systemd/system /etc/sudoers.d
  install -m 0755 "$pivot_root/target/release/censorpivot" /usr/local/bin/censorpivot
  install -m 0755 "$pivot_root/target/release/censord" /usr/local/bin/censord
  install -d -m 0750 /etc/agentcensor
  install -d -o root -g censorpivot -m 0750 /etc/censorpivot
  install_config "$pivot_root/censord.example.json" /etc/agentcensor/censord.json 0640 root censorpivot
  install_config "$pivot_root/config.example.json" /etc/censorpivot/config.json 0640 root censorpivot
  install -d -o root -g censorpivot -m 0750 /run/censorfs /run/censorguard
  install -d -m 0755 /run/censorscope
  install -d -o censorpivot -g censorpivot -m 0750 /var/lib/censorpivot/transactions /run/censorpivot
  install -d -o root -g root -m 0700 /run/censorpivot-cgroups
  if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
    mkdir -p /sys/fs/cgroup/censorpivot || warn "cannot create /sys/fs/cgroup/censorpivot; check cgroup v2 mount permissions"
  else
    warn "cgroup v2 is unavailable; CensorPivot tool runners cannot start"
  fi
  install -m 0644 "$pivot_root/deploy/systemd/agentcensord.service" /usr/lib/systemd/system/agentcensord.service
  install -m 0644 "$pivot_root/deploy/systemd/censorpivot.service" /usr/lib/systemd/system/censorpivot.service
  install -m 0440 "$pivot_root/deploy/sudoers/censorpivot" /etc/sudoers.d/censorpivot
  visudo -cf /etc/sudoers.d/censorpivot >/dev/null || fail \
    "generated CensorPivot sudoers rule is invalid" "remove /etc/sudoers.d/censorpivot and report this issue"
  install -m 0644 "$pivot_root/docs/INSTALL.zh-CN.md" /usr/share/doc/censorpivot/INSTALL.zh-CN.md
  systemctl daemon-reload
  info "CensorPivot installed for service user censorpivot"
}

component_services_stopped() {
  local service
  for service in censorfsd.service censorguardd.service; do
    if systemctl is-active --quiet "$service"; then
      fail "standalone service $service is already running" \
        "stop and disable it before unified start: sudo systemctl disable --now $service"
    fi
  done
  if /usr/sbin/censorscoped status 2>/dev/null | grep -q ' running pid='; then
    fail "a standalone CensorScope daemon is already running" \
      "stop it before unified start: sudo /usr/sbin/censorscoped stop"
  fi
}

require_cgroup_v2() {
  local mount=${1:-/sys/fs/cgroup}
  local controllers=$mount/cgroup.controllers
  local cgroup_root=$mount/censorpivot
  [[ -f "$controllers" ]] || fail "cgroup v2 is unavailable at $mount" \
    "enable or mount a writable unified cgroup v2 hierarchy, then rerun: sudo $script_dir/install-agentcensor.sh doctor"
  [[ -d "$mount" ]] || fail \
    "the cgroup v2 hierarchy is not mounted at $mount" \
    "mount a unified cgroup v2 hierarchy before starting AgentCensor"
  if [[ ! -d "$cgroup_root" ]]; then
    mkdir -p "$cgroup_root" || fail "cannot create CensorPivot cgroup root $cgroup_root" \
      "check that $mount is writable by root and rerun doctor"
  fi
  [[ -w "$cgroup_root" ]] || fail "CensorPivot cgroup root is not writable: $cgroup_root" \
    "check cgroup v2 mount permissions and rerun doctor"
}

start_systemd_with_diagnostics() {
  local service=$1
  shift
  if systemctl "$@" "$service"; then
    return 0
  fi
  printf '[ERROR] systemd could not start %s\n' "$service" >&2
  systemctl --no-pager --full status "$service" >&2 || true
  journalctl -u "$service" -n 100 --no-pager >&2 || true
  return 1
}

wait_for_data_plane() {
  local attempt
  for attempt in $(seq 1 45); do
    if /usr/local/bin/censord doctor --config /etc/agentcensor/censord.json >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  return 1
}

start_services() {
  require_root
  require_command systemctl "this start command requires systemd"
  require_component_file CensorFS /usr/libexec/censorfs/censorfsd fs
  require_component_file CensorGuard /usr/sbin/censorguardd guard
  require_component_file CensorScope /usr/sbin/censorscoped scope
  require_component_file CensorPivot /usr/local/bin/censord pivot
  require_cgroup_v2
  run_step "stopping any existing unified AgentCensor instance" \
    systemctl stop censorpivot.service agentcensord.service
  component_services_stopped
  run_step "enabling the AgentCensor services" systemctl enable agentcensord.service censorpivot.service
  if ! start_systemd_with_diagnostics agentcensord.service restart; then
    fail "starting the unified data-plane supervisor failed" \
      "fix the first error shown above, then rerun sudo $script_dir/install-agentcensor.sh doctor"
  fi
  if ! wait_for_data_plane; then
    systemctl --no-pager --full status agentcensord.service >&2 || true
    journalctl -u agentcensord.service -n 80 --no-pager >&2 || true
    fail "CensorFS, CensorGuard, and CensorScope did not become healthy" \
      "run sudo $script_dir/install-agentcensor.sh doctor and fix its first error"
  fi
  run_step "applying the default CensorGuard policy group" \
    /usr/bin/censorguardctl --socket /run/censorguard/ctl.sock policy apply \
    --name censorguard-dsh-default \
    --file /etc/censorguard/policy.d/censorguard-dsh-default.yaml
  if ! start_systemd_with_diagnostics censorpivot.service restart; then
    fail "starting CensorPivot failed" "fix the first error shown above, then rerun start"
  fi
  for _ in $(seq 1 20); do
    [[ -S /run/censorpivot/control.sock ]] && break
    sleep 0.25
  done
  [[ -S /run/censorpivot/control.sock ]] || {
    systemctl --no-pager --full status censorpivot.service >&2 || true
    journalctl -u censorpivot.service -n 80 --no-pager >&2 || true
    fail "CensorPivot did not create /run/censorpivot/control.sock" \
      "inspect the service log shown above"
  }
  /usr/local/bin/censord doctor --config /etc/agentcensor/censord.json
  runuser -u censorpivot -- /usr/local/bin/censorpivot doctor
  info "AgentCensor is running"
}

stop_services() {
  require_root
  require_command systemctl "this stop command requires systemd"
  systemctl stop censorpivot.service agentcensord.service
  info "AgentCensor services stopped"
}

status_services() {
  require_root
  systemctl --no-pager --full status agentcensord.service censorpivot.service || true
  [[ ! -x /usr/local/bin/censord ]] || /usr/local/bin/censord doctor --config /etc/agentcensor/censord.json || true
  [[ ! -x /usr/local/bin/censorpivot ]] || runuser -u censorpivot -- /usr/local/bin/censorpivot doctor || true
}

doctor_errors=0
doctor_check() {
  local label=$1 path=$2 repair=$3
  if [[ -e "$path" ]]; then
    printf '[OK] %s: %s\n' "$label" "$path"
  else
    printf '[ERROR] %s missing: %s\n[HINT] %s\n' "$label" "$path" "$repair" >&2
    doctor_errors=$((doctor_errors + 1))
  fi
}

doctor_cgroup_v2() {
  local controllers=/sys/fs/cgroup/cgroup.controllers
  local root=/sys/fs/cgroup/censorpivot
  if [[ ! -f "$controllers" ]]; then
    printf '[ERROR] cgroup v2 missing: %s\n[HINT] enable or mount a writable unified cgroup v2 hierarchy, then rerun doctor\n' \
      "$controllers" >&2
    doctor_errors=$((doctor_errors + 1))
    return
  fi
  printf '[OK] cgroup v2: %s\n' "$controllers"
  if [[ ! -d "$root" ]]; then
    printf '[ERROR] CensorPivot cgroup root missing: %s\n[HINT] run sudo %s/install-agentcensor.sh start after confirming the hierarchy is writable\n' \
      "$root" "$script_dir" >&2
    doctor_errors=$((doctor_errors + 1))
  elif [[ ! -w "$root" ]]; then
    printf '[ERROR] CensorPivot cgroup root is not writable: %s\n[HINT] check cgroup v2 mount permissions\n' \
      "$root" >&2
    doctor_errors=$((doctor_errors + 1))
  else
    printf '[OK] CensorPivot cgroup root writable: %s\n' "$root"
  fi
}

doctor_guard_runtime_dir() {
  local path=/run/censorguard
  if [[ ! -d "$path" ]]; then
    warn "$path is absent; systemd will create it when agentcensord starts"
    return
  fi
  local owner mode
  owner=$(stat -c '%u' "$path")
  mode=$(stat -c '%a' "$path")
  if [[ "$owner" != 0 || $((8#$mode & 8#022)) -ne 0 ]]; then
    printf '[ERROR] unsafe CensorGuard runtime directory: %s (uid=%s mode=%s)\n' \
      "$path" "$owner" "$mode" >&2
    printf '[HINT] reinstall the service unit: sudo %s/install-agentcensor.sh pivot\n' \
      "$script_dir" >&2
    doctor_errors=$((doctor_errors + 1))
  else
    printf '[OK] CensorGuard runtime directory is secure: %s (uid=%s mode=%s)\n' \
      "$path" "$owner" "$mode"
  fi
}

doctor_installation() {
  doctor_check "CensorFS daemon" /usr/libexec/censorfs/censorfsd \
    "sudo $script_dir/install-agentcensor.sh fs"
  doctor_check "CensorGuard daemon" /usr/sbin/censorguardd \
    "sudo $script_dir/install-agentcensor.sh guard"
  doctor_check "CensorScope daemon" /usr/sbin/censorscoped \
    "sudo $script_dir/install-agentcensor.sh scope"
  doctor_check "CensorPivot supervisor" /usr/local/bin/censord \
    "sudo $script_dir/install-agentcensor.sh pivot"
  doctor_check "censord configuration" /etc/agentcensor/censord.json \
    "sudo $script_dir/install-agentcensor.sh --force-config pivot"
  doctor_check "CensorPivot configuration" /etc/censorpivot/config.json \
    "sudo $script_dir/install-agentcensor.sh --force-config pivot"
  doctor_check "FUSE device" /dev/fuse "sudo modprobe fuse"
  doctor_check "kernel BTF" /sys/kernel/btf/vmlinux "install kernel BTF data or use a supported kernel"
  doctor_cgroup_v2
  doctor_guard_runtime_dir
  if [[ -r /sys/kernel/security/lsm ]] && grep -Eq '(^|,)bpf(,|$)' /sys/kernel/security/lsm; then
    printf '[OK] BPF LSM is enabled\n'
  else
    printf '[ERROR] BPF LSM is not enabled\n[HINT] add bpf to the kernel lsm= boot parameter and reboot\n' >&2
    doctor_errors=$((doctor_errors + 1))
  fi
  if getent passwd censorpivot >/dev/null 2>&1; then
    printf '[OK] service user censorpivot exists\n'
  else
    printf '[ERROR] service user censorpivot is missing\n[HINT] sudo %s/install-agentcensor.sh pivot\n' "$script_dir" >&2
    doctor_errors=$((doctor_errors + 1))
  fi
  if [[ $doctor_errors -eq 0 && -x /usr/local/bin/censord ]]; then
    if systemctl is-active --quiet agentcensord.service; then
      /usr/local/bin/censord doctor --config /etc/agentcensor/censord.json || doctor_errors=$((doctor_errors + 1))
    else
      warn "agentcensord.service is installed but not running; start with: sudo $script_dir/install-agentcensor.sh start"
    fi
  fi
  [[ $doctor_errors -eq 0 ]] || fail "$doctor_errors installation or kernel check(s) failed" \
    "apply the hints above, then rerun this doctor command"
  info "installation prerequisites are ready"
}

main() {
  if [[ ${1:-} == "--force-config" ]]; then
    force_config=true
    shift
  fi
  local action=${1:-help}
  [[ $# -le 1 ]] || { usage >&2; exit 2; }

  case "$action" in
    fs) install_fs ;;
    guard) install_guard ;;
    scope) install_scope ;;
    pivot) install_pivot ;;
    all)
      install_fs
      install_guard
      install_scope
      install_pivot
      ;;
    start) start_services ;;
    stop) stop_services ;;
    status) status_services ;;
    doctor) doctor_installation ;;
    help|-h|--help) usage ;;
    *) usage >&2; fail "unknown command: $action" "use one of: fs guard scope pivot all start stop status doctor" ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
