#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
installer="$root/scripts/install-agentcensor.sh"

bash -n "$installer"
help=$($installer help)
for command in fs guard scope pivot all start stop status doctor; do
  [[ "$help" == *"$command"* ]]
done

# Source the installer so the cgroup preflight can be exercised without touching
# the host hierarchy or invoking systemd.
source "$installer"
test_cgroup=$(mktemp -d)
trap 'rm -rf -- "$test_cgroup"' EXIT
if missing_output=$(require_cgroup_v2 "$test_cgroup" 2>&1); then
  printf 'missing cgroup v2 preflight unexpectedly succeeded\n' >&2
  exit 1
fi
[[ "$missing_output" == *"cgroup v2 is unavailable"* ]]
touch "$test_cgroup/cgroup.controllers"
require_cgroup_v2 "$test_cgroup"
[[ -d "$test_cgroup/censorpivot" ]]

node - "$root/censord.example.json" "$root/config.example.json" <<'NODE'
const fs = require('node:fs')
const daemon = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'))
const pivot = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'))
if (daemon.censorguard.policy !== '/etc/censorguard/agentcensor.yaml') process.exit(1)
if (!daemon.censorguard.extra_args.includes('--socket-group')) process.exit(1)
if (pivot.censorguard.exec_command !== '/usr/bin/censorguard-exec') process.exit(1)
if (pivot.censorscope.command !== '/usr/bin/censorscopectl') process.exit(1)
NODE

grep -q '^User=root$' "$root/deploy/systemd/agentcensord.service"
grep -q '^ConditionPathExists=/sys/fs/cgroup/cgroup.controllers$' "$root/deploy/systemd/agentcensord.service"
grep -q '^RuntimeDirectoryMode=0750$' "$root/deploy/systemd/agentcensord.service"
grep -q '^ExecStartPre=/usr/bin/mkdir -p /sys/fs/cgroup/censorpivot$' "$root/deploy/systemd/agentcensord.service"
grep -q '^User=censorpivot$' "$root/deploy/systemd/censorpivot.service"
grep -q '^censorpivot ALL=(root) NOPASSWD:' "$root/deploy/sudoers/censorpivot"
grep -q '^rules:' "$root/deploy/censorguard/agentcensor.yaml"
grep -q '^rules:' "$root/../CensorGuard/config/policy.dsh-default.yaml"

if command -v visudo >/dev/null 2>&1; then
  visudo -cf "$root/deploy/sudoers/censorpivot" >/dev/null
fi
if [[ -x "$root/../CensorGuard/target/release/censorguardd" && -f "$root/../CensorGuard/bpf/enforce.bpf.o" ]]; then
  "$root/../CensorGuard/target/release/censorguardd" \
    --config "$root/deploy/censorguard/agentcensor.yaml" \
    --bpf-object "$root/../CensorGuard/bpf/enforce.bpf.o" --check-config >/dev/null 2>&1
fi

printf 'AgentCensor native installer checks passed.\n'
