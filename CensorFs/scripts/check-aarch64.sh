#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

cargo +1.82.0 test --workspace --all-targets --locked
cargo +1.82.0 check --workspace --all-targets --target aarch64-unknown-linux-gnu --locked
