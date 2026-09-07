#!/usr/bin/env bash
set -euo pipefail

bin_dir=${CENSORFS_BIN_DIR:-/usr/local/bin}
archive=${CENSORFS_BIN_ARCHIVE:-}
url=${CENSORFS_RELEASE_URL:-}

if [[ -z "$archive" ]]; then
  [[ -n "$url" ]] || {
    echo "Set CENSORFS_RELEASE_URL to an openEuler/Linux release archive, or CENSORFS_BIN_ARCHIVE to a local archive." >&2
    exit 2
  }
  tmp_dir=$(mktemp -d)
  trap 'rm -rf "$tmp_dir"' EXIT
  archive="$tmp_dir/censorfs.tar.gz"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --retry 3 --output "$archive" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget --output-document="$archive" "$url"
  else
    echo "curl or wget is required to download CensorFS binaries" >&2
    exit 2
  fi
fi

[[ -f "$archive" ]] || { echo "binary archive not found: $archive" >&2; exit 2; }
mkdir -p "$bin_dir"
tmp_extract=$(mktemp -d)
trap 'rm -rf "$tmp_extract"' EXIT
tar -xzf "$archive" -C "$tmp_extract"

for binary in censorfs censorfsd censorfs-mounter; do
  source_path=$(find "$tmp_extract" -type f -name "$binary" -perm -u+x -print -quit)
  [[ -n "$source_path" ]] || { echo "archive does not contain executable $binary" >&2; exit 1; }
  install -m 0755 "$source_path" "$bin_dir/$binary"
done

printf 'Installed CensorFS binaries to %s\n' "$bin_dir"
"$bin_dir/censorfs" --version >/dev/null 2>&1 || true
"$bin_dir/censorfsd" --version >/dev/null 2>&1 || true
"$bin_dir/censorfs-mounter" --version >/dev/null 2>&1 || true
