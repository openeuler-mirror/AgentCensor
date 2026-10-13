#!/usr/bin/env bash
set -Eeuo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
plugin_dir="${repo_dir}/plugins/dsh-censorguard"
pnpm_bin=${PNPM:-pnpm}
command -v node >/dev/null
command -v npm >/dev/null
command -v "${pnpm_bin}" >/dev/null
if [[ ! -x ${plugin_dir}/node_modules/.bin/tsc || ! -x ${plugin_dir}/node_modules/.bin/esbuild ]]; then
    printf 'Preparing plugin dependencies from the local cache (run make deps if unavailable).\n'
    (cd -- "${plugin_dir}" && "${pnpm_bin}" install --frozen-lockfile --offline)
fi
(cd -- "${plugin_dir}" && "${pnpm_bin}" build)
mkdir -p -- "${repo_dir}/dist"
cache_dir=$(mktemp -d "${TMPDIR:-/tmp}/censorguard-pack.XXXXXX")
trap 'rm -rf -- "${cache_dir}"' EXIT
(cd -- "${plugin_dir}" && npm pack --ignore-scripts --cache "${cache_dir}" --pack-destination "${repo_dir}/dist" --silent)
printf 'Plugin package ready in %s/dist/\n' "${repo_dir}"
