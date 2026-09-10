#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: DSH_REAL_E2E_COMMAND='dsh ...' DSH_SESSION_SNAPSHOT=/path/events.json $0" >&2
  exit 2
}

[[ -n "${DSH_REAL_E2E_COMMAND:-}" && -n "${DSH_SESSION_SNAPSHOT:-}" ]] || usage
[[ "$(uname -s)" == Linux ]] || { echo 'real E2E requires Linux/openEuler' >&2; exit 2; }
[[ -c /dev/fuse ]] || { echo 'real E2E requires /dev/fuse' >&2; exit 2; }
for binary in censorfs censorfsd censorfs-mounter bwrap node jq; do
  command -v "$binary" >/dev/null || { echo "missing required command: $binary" >&2; exit 2; }
done
[[ -f /etc/os-release ]] && grep -Eiq '(^|\n)ID(_LIKE)?=.*(openEuler|openeuler)' /etc/os-release || {
  echo 'warning: target is not identified as openEuler; continue only with explicit release approval' >&2
}

cgroup_type=$(stat -f -c %T /sys/fs/cgroup 2>/dev/null || true)
[[ "$cgroup_type" == cgroup2fs ]] || {
  echo 'real E2E requires a cgroup v2 mount; cgroup v1 is unsupported' >&2
  exit 2
}

snapshot_dir=$(dirname "$DSH_SESSION_SNAPSHOT")
mkdir -p "$snapshot_dir"
printf 'Running real branch_explore_inprocess E2E command...\n'
# The operator supplies the exact DSH invocation so this script never guesses
# profile, credentials, or the host-specific session export mechanism.
eval "$DSH_REAL_E2E_COMMAND"
[[ -s "$DSH_SESSION_SNAPSHOT" ]] || { echo "missing session snapshot: $DSH_SESSION_SNAPSHOT" >&2; exit 1; }

jq -e '
  . as $events
  | any(.[]; .type == "exploration-started" and .data.mode == "in-process")
  and ([.[] | select(.type == "variant-running")] | length >= 2 and length <= 4)
  and any(.[]; .type == "ranking-ready")
  and any(.[]; .type == "exploration-ended")
  and any(.[]; .type == "variant-published")
' "$DSH_SESSION_SNAPSHOT" >/dev/null || {
  echo 'session snapshot does not prove in-process Compare -> Publish completion' >&2
  exit 1
}

printf 'PASS: real branch_explore_inprocess completed with 2-4 variants and publish evidence\n'
printf 'Session evidence: %s\n' "$DSH_SESSION_SNAPSHOT"
