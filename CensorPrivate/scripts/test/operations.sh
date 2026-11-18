#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
dsh_test_root=
static_only=false
if [[ $# == 2 && $1 == --dsh-root ]]; then dsh_test_root=$2
elif [[ $# == 1 && $1 == --static ]]; then static_only=true
elif [[ $# != 0 ]]; then
  printf 'Usage: bash scripts/test/operations.sh [--static|--dsh-root /path/to/deepseek-harness]\n' >&2; exit 2
fi
for file in cpctl scripts/ops/*.sh scripts/release/*.sh scripts/install.sh scripts/package.sh; do bash -n "$file"; done
for file in scripts/ops/*.mjs scripts/release/*.mjs plugins/dsh-censorprivacy/*.mjs plugins/detector-web/app.js scripts/test/detector-web.mjs scripts/test/standalone.mjs scripts/test/dsh-web.mjs scripts/test/xiaoo-gateway.mjs; do node --check "$file"; done
if $static_only; then
  # Fresh-clone CI can validate adapters without provisioning 400 MB weights.
  node --test tests/dsh-plugin.test.mjs
  exit 0
fi
# Build before timed lifecycle assertions; a cold LTO link can exceed their
# timeout while installation and daemon startup themselves remain responsive.
cargo build --locked --release --bins
CP_TEST_DSH_ROOT="$dsh_test_root" node --test tests/dsh-plugin.test.mjs tests/operations.test.mjs
if [[ -n $dsh_test_root ]]; then
  dsh_demo_config=$(mktemp -d)
  trap 'rm -rf -- "$dsh_demo_config"' EXIT
  # Keep an older user/system install from replacing the binaries just built.
  ./cpctl demo dsh-gateway --dsh-root "$dsh_test_root" --config-dir "$dsh_demo_config" --prefix "$PWD/target"
  ./cpctl demo dsh-tls --dsh-root "$dsh_test_root" --config-dir "$dsh_demo_config" --prefix "$PWD/target"
fi
