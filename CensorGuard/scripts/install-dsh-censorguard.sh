#!/usr/bin/env bash
set -Eeuo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
harness_dir=${1:?usage: install-dsh-censorguard.sh HARNESS_ROOT [PROFILE]}
profile=${2:-web}

if [[ ! -d "${harness_dir}" ]]; then
    printf 'Harness directory does not exist: %s\n' "${harness_dir}" >&2
    exit 2
fi
command -v npm >/dev/null || { printf 'npm is required\n' >&2; exit 2; }
command -v pnpm >/dev/null || { printf 'pnpm is required\n' >&2; exit 2; }

plugin_dir="${repo_dir}/plugins/dsh-censorguard"
cache_dir=$(mktemp -d "${TMPDIR:-/tmp}/Censorguard-plugin-cache.XXXXXX")
trap 'rm -rf -- "${cache_dir}"' EXIT

if [[ -x "${plugin_dir}/node_modules/.bin/tsc" && -x "${plugin_dir}/node_modules/.bin/esbuild" ]]; then
    printf '[1/3] reusing existing plugin build dependencies\n'
else
    printf '[1/3] installing plugin build dependencies offline\n'
    npm --prefix "${plugin_dir}" ci --ignore-scripts --offline
fi
printf '[2/3] building plugin package\n'
npm --prefix "${plugin_dir}" run build
output_dir="${repo_dir}/dist"
mkdir -p "${output_dir}"
tarball=$(cd "${plugin_dir}" && npm_config_cache="${cache_dir}" npm pack --pack-destination "${output_dir}" --silent | tail -n 1)
tarball="${output_dir}/${tarball##*/}"
printf '[3/3] adding %s to Harness %s profile\n' "${tarball}" "${profile}"
# Remove an older direct file dependency first.  This also repairs profiles that still point
# at a deleted temporary tarball from an interrupted or older installer run.
profile_dir="${DSH_HOME:-${HOME}/.dsh}/profiles/${profile}"
if [[ -f "${profile_dir}/package.json" ]] && grep -q '"dsh-censorguard"' "${profile_dir}/package.json"; then
    (cd "${harness_dir}" && pnpm dsh plugin --profile "${profile}" remove dsh-censorguard)
fi
(cd "${harness_dir}" && pnpm dsh plugin --profile "${profile}" add "${tarball}")

printf 'dsh-censorguard installed in profile %s\n' "${profile}"
