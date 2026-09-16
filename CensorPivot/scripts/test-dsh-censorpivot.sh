#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
output=$(mktemp -d "${TMPDIR:-/tmp}/censorpivot-dsh-test.XXXXXX")
trap 'rm -rf -- "$output"' EXIT

node --test "$root/plugins/dsh-censorpivot/test/composition.test.mjs"
bash -n "$root/scripts/package-dsh-censorpivot.sh"
bash -n "$root/scripts/install-dsh-censorpivot.sh"
"$root/scripts/package-dsh-censorpivot.sh" "$output/packages"

expected=(
  censorpivot-web.tgz censorpivot-headless.tgz
  censorfs-web.tgz censorfs-headless.tgz censorguard-web.tgz
  censorscope-host.tgz censorscope-ui.tgz censorscope-session-proxy.tgz
)
for archive in "${expected[@]}"; do
  test -s "$output/packages/$archive"
  test -s "$output/packages/$archive.sha256"
done

for runtime in censorfs-web censorfs-headless censorguard-web censorscope-host censorscope-ui censorscope-session-proxy; do
  tar -xOf "$output/packages/$runtime.tgz" package/package.json | \
    node -e 'let s="";process.stdin.on("data",c=>s+=c).on("end",()=>{const p=JSON.parse(s);if(p.dsh?.bundle)process.exit(1)})'
done

for runtime in censorfs-web censorguard-web censorscope-ui; do
  tar -xOf "$output/packages/$runtime.tgz" package/package.json | \
    node -e 'let s="";process.stdin.on("data",c=>s+=c).on("end",()=>{const p=JSON.parse(s);if(!p.dsh?.client)process.exit(1)})'
done

proxy_source=$(tar -xOf "$output/packages/censorscope-session-proxy.tgz" package/lib/index.mjs)
[[ "$proxy_source" != *"import { turnBoundaryProjectionDefinition }"* ]]
[[ "$proxy_source" == *"turnBoundary projection unavailable; compatibility mode"* ]]
[[ "$proxy_source" == *"prefixArgs: ['--dir', candidate, 'dsh']"* ]]

censorfs_client=$(tar -xOf "$output/packages/censorfs-web.tgz" package/client/index.js)
[[ "$censorfs_client" == *"'uiConversation'"* ]]
[[ "$censorfs_client" == *"ctx.uiConversation.events.register"* ]]
[[ "$censorfs_client" == *"ctx.uiConversation.views.register"* ]]
[[ "$censorfs_client" != *"conversationEvents"* ]]
[[ "$censorfs_client" != *"conversationViews"* ]]

proxy_source=$(tar -xOf "$output/packages/censorscope-session-proxy.tgz" package/lib/index.mjs)
[[ "$proxy_source" == *"persistence.open(id, 'write')"* ]]
[[ "$proxy_source" == *"const loaded = await handle.read()"* ]]
[[ "$proxy_source" != *"persistence.prepare(id)"* ]]
printf 'CensorPivot DSH composite checks passed.\n'
