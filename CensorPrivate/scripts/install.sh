#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
prefix=/usr/local
destdir=${DESTDIR:-}
with_native=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) prefix=${2:?prefix required}; shift 2 ;;
    --with-native) with_native=true; shift ;;
    *) printf 'Usage: bash scripts/install.sh [--prefix /usr/local] [--with-native]\n' >&2; exit 2 ;;
  esac
done
[[ "$prefix" == /* ]] || { printf 'prefix must be absolute\n' >&2; exit 2; }
cargo build --locked --release --bins
install -d -m 0755 "$destdir$prefix/bin"
for binary in censorprivacy privacy-run privcalc censorprivacy-mcp tls-run; do
  install -m 0755 "${CARGO_TARGET_DIR:-target}/release/$binary" "$destdir$prefix/bin/$binary"
done
install -m 0755 cpctl "$destdir$prefix/bin/cpctl"
if $with_native; then
  make -C native
  install -d -m 0755 "$destdir$prefix/lib/censorprivacy"
  install -m 0755 native/libcp_tls_guard.so "$destdir$prefix/lib/censorprivacy/libcp_tls_guard.so"
fi
install -d -m 0755 "$destdir$prefix/share/censorprivacy"
install -m 0644 examples/policy.yaml "$destdir$prefix/share/censorprivacy/policy.example.yaml"
for directory in scripts/ops plugins/dsh-censorprivacy docs deploy examples native; do
  install -d -m 0755 "$destdir$prefix/share/censorprivacy/$directory"
done
for file in scripts/ops/*.sh scripts/ops/*.mjs plugins/dsh-censorprivacy/*.mjs plugins/dsh-censorprivacy/*.json plugins/dsh-censorprivacy/*.yml plugins/dsh-censorprivacy/README.md docs/*.md deploy/censorprivacy@.service examples/policy.yaml native/README.md README.md CONTRIBUTING.md LICENSE; do
  install -m 0644 "$file" "$destdir$prefix/share/censorprivacy/$file"
done
printf 'Installed Rust commands to %s\n' "$destdir$prefix/bin"
