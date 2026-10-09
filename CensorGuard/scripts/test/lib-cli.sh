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
  audit_exec: ${TEST_AUDIT_EXEC:-false}
  audit_file: ${TEST_AUDIT_FILE:-false}
  audit_net: ${TEST_AUDIT_NET:-false}
YAML
        if [[ -n ${socket_group} ]]; then
            printf '  socket_group: %s\n' "${socket_group}"
        fi
        # Public startup input is always daemon + base rules only.
        awk '
            /^[^[:space:]#]/ { keep = /^rules:/ }
            keep { print }
        ' "${policy}"
    } >"${output}"
    # Old multi-domain fixtures model private persisted state, not public config.
    # Keep revision 1 for lifecycle/capacity/rollback probes. New publication and
    # binding behavior is exercised through the CLI in integration-policy-entry.py.
    if [[ ! -f ${state_dir}/current ]] && grep -Eq '^(groups|domains):' "${policy}"; then
        install -d -m 0700 "${state_dir}/revisions"
        write_test_snapshot "${output}" "${policy}" "${state_dir}/revisions/1.yaml"
        printf '1\n' >"${state_dir}/current"
    fi
}

test_ctl() {
    local config=$1
    shift
    censorguardctl --config "${config}" "$@"
}

# Build a private snapshot with the same daemon settings as the public config.
write_test_snapshot() {
    local config=$1 policy=$2 output=$3
    {
        awk '
            /^daemon:/ { keep=1 }
            keep && /^[^[:space:]#]/ && !/^daemon:/ { keep=0 }
            keep { print }
        ' "${config}"
        awk '
            /^daemon:/ { skip=1; next }
            skip && /^[^[:space:]#]/ { skip=0 }
            !skip { print }
        ' "${policy}"
    } >"${output}"
}
