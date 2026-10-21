#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
native=false
runtime=false
operations=false
for option in "$@"; do
  case "$option" in
    --native) native=true ;;
    --runtime) runtime=true ;;
    --operations) operations=true ;;
    *) printf 'Usage: bash scripts/test/run-all.sh [--native] [--runtime] [--operations]\n' >&2; exit 2 ;;
  esac
done
bash scripts/test/rust.sh
if $native; then bash scripts/test/native.sh; fi
if $runtime; then bash scripts/test/runtime.sh; fi
if $operations; then bash scripts/test/operations.sh; fi
