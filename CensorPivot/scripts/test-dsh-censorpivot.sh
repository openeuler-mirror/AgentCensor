#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
output=$(mktemp -d "${TMPDIR:-/tmp}/censorpivot-dsh-test.XXXXXX")
trap 'rm -rf -- "$output"' EXIT

node --test "$root/plugins/dsh-censorpivot/test/composition.test.mjs"
bash -n "$root/scripts/package-dsh-censorpivot.sh"
bash -n "$root/scripts/install-dsh-censorpivot.sh"
DSH_PNPM_STORE_DIR="$output/pnpm-store" \
  "$root/scripts/package-dsh-censorpivot.sh" "$output/packages"

expected=(
  censorpivot-web.tgz censorpivot-headless.tgz
  censorfs-web.tgz censorfs-headless.tgz censorguard-web.tgz
  censorscope-host.tgz censorscope-session-proxy.tgz
)
for archive in "${expected[@]}"; do
  test -s "$output/packages/$archive"
  test -s "$output/packages/$archive.sha256"
done

test ! -e "$output/packages/censorscope-ui.tgz"

for runtime in censorfs-web censorfs-headless censorguard-web censorscope-host censorscope-session-proxy; do
  tar -xOf "$output/packages/$runtime.tgz" package/package.json | \
    node -e 'let s="";process.stdin.on("data",c=>s+=c).on("end",()=>{const p=JSON.parse(s);if(p.dsh?.bundle)process.exit(1)})'
done

for runtime in censorfs-web censorguard-web; do
  tar -xOf "$output/packages/$runtime.tgz" package/package.json | \
    node -e 'let s="";process.stdin.on("data",c=>s+=c).on("end",()=>{const p=JSON.parse(s);if(!p.dsh?.client)process.exit(1)})'
done

for entry in \
  package/dist/bootstrap/index.js \
  package/dist/host/index.js \
  package/dist/ui/index.js \
  package/lib/client.js; do
  tar -tzf "$output/packages/censorguard-web.tgz" "$entry" >/dev/null
done

proxy_source=$(tar -xOf "$output/packages/censorscope-session-proxy.tgz" package/lib/index.mjs)
[[ "$proxy_source" != *"import { turnBoundaryProjectionDefinition }"* ]]
[[ "$proxy_source" == *"turnBoundary projection unavailable; compatibility mode"* ]]
[[ "$proxy_source" == *"prefixArgs: ['--dir', candidate, 'dsh']"* ]]
if [[ -n ${DSH_ROOT:-} ]]; then
  node "$root/scripts/verify-dsh-profile.mjs" "$output/packages" "$DSH_ROOT"
fi
printf 'CensorPivot DSH composite checks passed.\n'
