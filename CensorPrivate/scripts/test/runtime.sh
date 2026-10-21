#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
[[ $(id -u) -eq 0 ]] || { printf 'Linux root with namespace/mount/nft capabilities required\n' >&2; exit 2; }
cargo build --locked --example runtime_probe
cargo test --locked --test runtime_linux -- --ignored --nocapture
