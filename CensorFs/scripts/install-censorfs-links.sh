#!/usr/bin/env bash
set -euo pipefail

bin_dir=${1:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../target/release" && pwd)"}
binary="$bin_dir/censorfs"

if [[ ! -x "$binary" ]]; then
  echo "missing executable: $binary" >&2
  echo "build it first with: cargo build --release --locked -p censorfs" >&2
  exit 2
fi

commands=(
  init daemon info branch-create branch-list head explore
  ls cat write mkdir rm mv commit abort diff merge generation fsck
)
for command in "${commands[@]}"; do
  ln -sfn censorfs "$bin_dir/censorfs-$command"
done

echo "CensorFS command links installed in: $bin_dir"
