#!/usr/bin/env bash
set -Eeuo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
harness_dir=${1:?usage: install-dsh-censorguard.sh HARNESS_ROOT [PROFILE]}
profile=${2:-web}

if [[ ! -d "${harness_dir}" ]]; then
    printf 'Harness directory does not exist: %s\n' "${harness_dir}" >&2
    exit 2
fi
command -v pnpm >/dev/null || { printf 'pnpm is required\n' >&2; exit 2; }

printf '[1/2] Building the plugin package\n'
bash "${repo_dir}/scripts/build-dsh-plugin.sh"
plugin_version=$(node -p 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).version' "${repo_dir}/plugins/dsh-censorguard/package.json")
tarball="${repo_dir}/dist/censorguard-dsh-${plugin_version}.tgz"
printf '[2/2] Adding %s to Harness %s profile\n' "${tarball}" "${profile}"
# Remove obsolete dependencies in one pass, including dangling old tarball paths.
profile_dir="${DSH_HOME:-${HOME}/.dsh}/profiles/${profile}"
legacy_packages=()
for legacy in dsh-censorguard @censorguard/dsh-bundle @censorguard/dsh-bootstrap @censorguard/dsh-host @censorguard/dsh-client; do
    if [[ -f "${profile_dir}/package.json" ]] && grep -Fq "\"${legacy}\"" "${profile_dir}/package.json"; then
        legacy_packages+=("${legacy}")
    fi
done
if (( ${#legacy_packages[@]} )); then
    (cd "${harness_dir}" && pnpm dsh plugin --profile "${profile}" remove --ignore-scripts "${legacy_packages[@]}")
fi
# The tarball is fully built and its runtime dependencies are pure JavaScript.
# A new profile has its own pnpm build policy; protobufjs's optional postinstall
# must not turn an otherwise complete installation into ERR_PNPM_IGNORED_BUILDS.
(cd "${harness_dir}" && pnpm dsh plugin --profile "${profile}" add --ignore-scripts "${tarball}")

printf '@censorguard/dsh installed in profile %s\n' "${profile}"
