#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
node plugins/dsh-censorprivacy/scripts/build-client.mjs
mkdir -p dist
# npm is only used for its standard package format; no registry access or install.
cache_dir=$(mktemp -d "${TMPDIR:-/tmp}/censorprivacy-pack.XXXXXX")
trap 'rm -rf -- "$cache_dir"' EXIT
npm pack ./plugins/dsh-censorprivacy --ignore-scripts --cache "$cache_dir" --pack-destination "$PWD/dist"
