#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out_dir=${1:-${CENSORFS_PACKAGE_DIR:-$root/dist/explore}}
mkdir -p "$out_dir"

pack_one() {
  local directory=$1
  local name=$2
  local staging
  staging=$(mktemp -d)
  trap 'rm -rf "$staging"' RETURN
  (cd "$directory" && pnpm pack --pack-destination "$staging" >/dev/null)
  local archive
  archive=$(find "$staging" -maxdepth 1 -type f -name '*.tgz' -print -quit)
  [[ -n "$archive" ]] || { echo "pnpm pack produced no archive for $name" >&2; exit 1; }
  local destination="$out_dir/$(basename "$archive")"
  install -m 0644 "$archive" "$destination"
  sha256sum "$destination" > "$destination.sha256"
  printf 'PACKED %s %s\n' "$name" "$destination"
}

pack_one "$root/integrations/deepseek-harness" deepseek-harness
pack_one "$root/integrations/deepseek-harness/event-exporter" event-exporter
printf 'Package directory: %s\n' "$out_dir"
