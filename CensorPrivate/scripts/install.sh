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
install -m 0755 "${CARGO_TARGET_DIR:-target}/release/censorprivacy" "$destdir$prefix/bin/censorprivacy"
if $with_native; then
  make -C native
  install -d -m 0755 "$destdir$prefix/lib/censorprivacy"
  install -m 0755 native/libcensorprivacy_tls_guard.so "$destdir$prefix/lib/censorprivacy/libcensorprivacy_tls_guard.so"
fi
install -d -m 0755 "$destdir$prefix/share/censorprivacy"
if [[ ! -e $destdir$prefix/share/censorprivacy/config.yaml && ! -L $destdir$prefix/share/censorprivacy/config.yaml ]]; then
  # Installed deployments must not inherit paths into the source user's home.
  "${CARGO_TARGET_DIR:-target}/release/censorprivacy" config-check --config config.yaml --print-resolved |
    jq '.server.token_file = null | .server.upstream_key_file = null |
        .server.upstream_ca_file = null | .server.tls_cert = null | .server.tls_key = null |
        .server.run_state_file = null | .ner.model_dir = null | .ner.ort_library = null |
        .private_tools_file = null | .conversation_records_dir = null' > "$destdir$prefix/share/censorprivacy/config.yaml"
  chmod 0644 "$destdir$prefix/share/censorprivacy/config.yaml"
fi
for directory in scripts/ops plugins/dsh-censorprivacy plugins/dsh-censorprivacy/lib plugins/detector-web docs deploy examples/private-tools native; do
  install -d -m 0755 "$destdir$prefix/share/censorprivacy/$directory"
done
for file in scripts/ops/*.sh scripts/ops/*.mjs plugins/dsh-censorprivacy/*.mjs plugins/dsh-censorprivacy/*.json plugins/dsh-censorprivacy/*.yml plugins/dsh-censorprivacy/lib/client.js plugins/dsh-censorprivacy/README.md plugins/detector-web/README.md docs/*.md deploy/censorprivacy@.service examples/*.txt examples/private-tools/* native/README.md README.md; do
  install -m 0644 "$file" "$destdir$prefix/share/censorprivacy/$file"
done
# Core inference has no Python worker. Optional private-tool examples are scripts.
# Source documentation links to the repository license; installed docs use the copy.
sed -i 's|](../LICENSE)|](LICENSE)|g' "$destdir$prefix/share/censorprivacy/README.md"
install -m 0644 ../LICENSE "$destdir$prefix/share/censorprivacy/LICENSE"
printf 'Installed Rust commands to %s\n' "$destdir$prefix/bin"
printf 'Provision the local ONNX bundle and ONNX Runtime: see docs/rust-detector.md\n'
