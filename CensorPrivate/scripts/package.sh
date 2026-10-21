#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
with_native=false
if [[ "${1:-}" == --with-native && $# == 1 ]]; then with_native=true
elif [[ $# -gt 0 ]]; then printf 'Usage: bash scripts/package.sh [--with-native]\n' >&2; exit 2; fi
package_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
package_arch=$(uname -m)
package_name="censorprivacy-$package_version-linux-$package_arch"
staging=$(mktemp -d)
trap 'rm -rf -- "$staging"' EXIT
if $with_native; then DESTDIR="$staging/$package_name" bash scripts/install.sh --prefix /usr/local --with-native
else DESTDIR="$staging/$package_name" bash scripts/install.sh --prefix /usr/local; fi
mkdir -p dist
tar -C "$staging" -czf "dist/$package_name.tar.gz" "$package_name"
sha256sum "dist/$package_name.tar.gz" > "dist/$package_name.tar.gz.sha256"
printf 'Created dist/%s.tar.gz\n' "$package_name"
