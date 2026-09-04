#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=${1:?usage: package.sh VERSION ARCH OUTPUT_DIR}
architecture=${2:?usage: package.sh VERSION ARCH OUTPUT_DIR}
output_dir=${3:?usage: package.sh VERSION ARCH OUTPUT_DIR}
source_date_epoch=${SOURCE_DATE_EPOCH:-0}

if [[ ! ${version} =~ ^[0-9A-Za-z.+~-]+$ ]]; then
    echo "unsafe package version: ${version}" >&2
    exit 2
fi
if [[ ! ${architecture} =~ ^[0-9A-Za-z_.-]+$ ]]; then
    echo "unsafe package architecture: ${architecture}" >&2
    exit 2
fi
if [[ ! ${source_date_epoch} =~ ^[0-9]+$ ]]; then
    echo "SOURCE_DATE_EPOCH must be an unsigned integer" >&2
    exit 2
fi

stage=$(mktemp -d /tmp/censorguard-package.XXXXXX)
cleanup() {
    case ${stage} in
        /tmp/censorguard-package.*) rm -rf -- "${stage}" ;;
        *) echo "refusing to remove unexpected staging path: ${stage}" >&2 ;;
    esac
}
trap cleanup EXIT

make --no-print-directory -C "${repo_dir}" install-files DESTDIR="${stage}"
# A plain tar archive has no package-manager "config(noreplace)" semantics. Ship only an example
# so extracting an upgrade can never overwrite the administrator's active policy.
# Policy samples are versioned and never overwrite an administrator's files.
for policy in base standard-dev high-priv; do
    mv "${stage}/etc/censorguard/${policy}.yaml" \
       "${stage}/etc/censorguard/${policy}.yaml.example"
done
install -d -m 0755 "${output_dir}"
package_name="Censorguard-${version}-${architecture}"
archive="${output_dir}/${package_name}.tar.gz"

tar --format=gnu --sort=name --mtime="@${source_date_epoch}" \
    --owner=0 --group=0 --numeric-owner -C "${stage}" -cf - . | gzip -n >"${archive}"
(
    cd -- "${output_dir}"
    sha256sum "${package_name}.tar.gz" >"${package_name}.tar.gz.sha256"
)
printf 'created %s\n' "${archive}"
