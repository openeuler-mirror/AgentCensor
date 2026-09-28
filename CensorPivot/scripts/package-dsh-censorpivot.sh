#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo_root=$(cd "$root/.." && pwd)
out_dir=${1:-${CENSORPIVOT_PACKAGE_DIR:-$root/dist/dsh-censorpivot}}
work=$(mktemp -d "${TMPDIR:-/tmp}/censorpivot-dsh-package.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
export npm_config_cache=${NPM_CACHE_DIR:-$work/npm-cache}

censorfs_root=${CENSORFS_ROOT:-$repo_root/CensorFs}
censorguard_root=${CENSORGUARD_ROOT:-$repo_root/CensorGuard}
censorscope_root=${CENSORSCOPE_ROOT:-$repo_root/CensorScope}

for command in node npm tar sha256sum; do
  command -v "$command" >/dev/null || { printf '%s is required\n' "$command" >&2; exit 2; }
done

rm -rf -- "$out_dir"
mkdir -p "$out_dir" "$work/raw" "$work/runtime"

pack_raw() {
  local source=$1 label=$2 raw_dir archive
  [[ -f "$source/package.json" ]] || { printf '%s package missing: %s\n' "$label" "$source" >&2; exit 2; }
  raw_dir=$(mktemp -d "$work/raw/${label}.XXXXXX")
  (cd "$source" && npm pack --pack-destination "$raw_dir" --silent >/dev/null)
  archive=$(find "$raw_dir" -maxdepth 1 -type f -name '*.tgz' -print -quit)
  [[ -n "$archive" ]] || { printf 'npm pack produced no archive for %s\n' "$label" >&2; exit 1; }
  printf '%s\n' "$archive"
}

# Runtime packages keep their code and dsh.client metadata, but the individual
# bundle declaration is removed. Their patches are owned by CensorPivot.
pack_runtime() {
  local source=$1 destination=$2 label=$3 raw extract packed_dir archive
  raw=$(pack_raw "$source" "$label")
  extract=$(mktemp -d "$work/runtime/${label}.XXXXXX")
  tar -xzf "$raw" -C "$extract"
  node - "$extract/package/package.json" <<'NODE'
const fs = require('node:fs')
const path = process.argv[2]
const pkg = JSON.parse(fs.readFileSync(path, 'utf8'))
if (pkg.dsh && pkg.dsh.bundle) delete pkg.dsh.bundle
if (pkg.dsh && Object.keys(pkg.dsh).length === 0) delete pkg.dsh
fs.writeFileSync(path, `${JSON.stringify(pkg, null, 2)}\n`)
NODE
  packed_dir=$(mktemp -d "$work/runtime-packed.${label}.XXXXXX")
  (cd "$extract/package" && npm pack --pack-destination "$packed_dir" --silent >/dev/null)
  archive=$(find "$packed_dir" -maxdepth 1 -type f -name '*.tgz' -print -quit)
  [[ -n "$archive" ]] || { printf 'npm pack produced no runtime archive for %s\n' "$label" >&2; exit 1; }
  install -m 0644 "$archive" "$out_dir/$destination"
}

pack_meta() {
  local source=$1 destination=$2 label=$3 archive
  archive=$(pack_raw "$source" "$label")
  install -m 0644 "$archive" "$out_dir/$destination"
}

pack_runtime "$censorfs_root/integrations/deepseek-harness" censorfs-web.tgz censorfs-web
pack_runtime "$censorfs_root/integrations/deepseek-harness/event-exporter" censorfs-headless.tgz censorfs-headless
pack_runtime "$censorguard_root/plugins/dsh-censorguard" censorguard-web.tgz censorguard-web
pack_runtime "$censorscope_root/plugins/censorscope-host" censorscope-host.tgz censorscope-host
pack_runtime "$censorscope_root/plugins/censorscope-ui" censorscope-ui.tgz censorscope-ui
pack_runtime "$censorscope_root/plugins/agentcensor-session-proxy" censorscope-session-proxy.tgz censorscope-session-proxy
pack_meta "$root/plugins/dsh-censorpivot" censorpivot-web.tgz censorpivot-web
pack_meta "$root/plugins/dsh-censorpivot/headless" censorpivot-headless.tgz censorpivot-headless

for archive in "$out_dir"/*.tgz; do
  sha256sum "$archive" > "$archive.sha256"
done
printf 'CensorPivot DSH packages: %s\n' "$out_dir"
