#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
case "${1:-}" in
  '') mode=rules ;;
  --integration) mode=integration ;;
  *) printf 'Usage: bash scripts/test/detector.sh [--integration]\n' >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || exit 2
cargo test --locked --lib professional::
if [[ $mode == integration ]]; then
  cargo test --locked --test professional_gateway -- --ignored --nocapture --test-threads=1
  cargo test --locked --test https_cli -- --ignored --nocapture
fi
