#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
pivot_root=$(cd -- "${script_dir}/.." && pwd)

demo_bin=${CENSORPIVOT_DEMO_BIN:-${pivot_root}/target/release/censorpivot-demo}
censorfs_bin=${CENSORFS_BIN:-censorfs}
pivot_socket=${CENSORPIVOT_SOCKET:-/run/censorpivot/control.sock}
censorfs_socket=${CENSORFS_SOCKET:-/run/censorfs/control.sock}
branch=${CENSORPIVOT_DEMO_BRANCH:-main}
guard_group=${CENSORPIVOT_DEMO_GUARD_GROUP:-censorguard-dsh-default}

for command in "$demo_bin" "$censorfs_bin"; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'demo: required command not found: %s\n' "$command" >&2
        exit 1
    fi
done

exec "$demo_bin" \
    --pivot-socket "$pivot_socket" \
    --censorfs-socket "$censorfs_socket" \
    --censorfs-command "$censorfs_bin" \
    --branch "$branch" \
    --guard-group "$guard_group"
