#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
dsh_test_root=
if [[ $# == 2 && $1 == --dsh-root ]]; then dsh_test_root=$2
elif [[ $# != 0 ]]; then
  printf 'Usage: bash scripts/test/operations.sh [--dsh-root /path/to/deepseek-harness]\n' >&2; exit 2
fi
for file in cpctl scripts/ops/*.sh scripts/install.sh scripts/package.sh; do bash -n "$file"; done
for file in scripts/ops/*.mjs plugins/dsh-censorprivacy/*.mjs; do node --check "$file"; done
# Build before timed lifecycle assertions; a cold LTO link can exceed their
# timeout while installation and daemon startup themselves remain responsive.
cargo build --locked --release --bins
CP_TEST_DSH_ROOT="$dsh_test_root" node --test tests/dsh-plugin.test.mjs tests/operations.test.mjs
if [[ -n $dsh_test_root ]]; then
  ./cpctl demo dsh-gateway --dsh-root "$dsh_test_root"
  ./cpctl demo dsh-tls --dsh-root "$dsh_test_root"
fi
