#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
native=false
runtime=false
operations=false
detector=false
detector_integration=false
for option in "$@"; do
  case "$option" in
    --native) native=true ;;
    --runtime) runtime=true ;;
    --operations) operations=true ;;
    --detector) detector=true ;;
    --detector-integration) detector=true; detector_integration=true ;;
    *) printf 'Usage: bash scripts/test/run-all.sh [--native] [--runtime] [--operations] [--detector] [--detector-integration]\n' >&2; exit 2 ;;
  esac
done
bash scripts/test/rust.sh
if $native; then bash scripts/test/native.sh; fi
if $runtime; then bash scripts/test/runtime.sh; fi
if $operations; then bash scripts/test/operations.sh; fi

if $detector; then
  if $detector_integration; then bash scripts/test/detector.sh --integration
  else bash scripts/test/detector.sh; fi
fi
