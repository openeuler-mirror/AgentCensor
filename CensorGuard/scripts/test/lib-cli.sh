#!/usr/bin/env bash

# Shared helpers for integration tests.  Runtime paths belong to the same
# configuration document consumed by censorguardd and censorguardctl.
write_test_config() {
    local output=$1 policy=$2 bpf=$3 runtime=$4
    # Fixed runtime directories must not restore a previous test's policy.
    # Persistence tests pass an explicit directory to retain revisions.
    local state_dir=${5:-${runtime}/state-$$}
    local socket_group=${6:-}
    local dns_refresh=${7:-60}
    {
        cat <<YAML
daemon:
  bpf_object: ${bpf}
  control_socket: ${runtime}/ctl.sock
  event_socket: ${runtime}/events.sock
  launch_socket: ${runtime}/launch.sock
  dsh_socket: ${runtime}/dsh.sock
  ui_socket: ${runtime}/ui.sock
  state_dir: ${state_dir}
  dns_refresh_seconds: ${dns_refresh}
YAML
        if [[ -n ${socket_group} ]]; then
            printf '  socket_group: %s\n' "${socket_group}"
        fi
        # Existing fixtures sometimes use config/base.yaml.  Remove its
        # daemon section before appending the policy portion.
        awk '
            /^daemon:[[:space:]]*$/ { skip=1; next }
            skip && /^[^[:space:]#]/ { skip=0 }
            !skip { print }
        ' "${policy}"
    } >"${output}"
}

test_ctl() {
    local config=$1
    shift
    censorguardctl --config "${config}" "$@"
}
