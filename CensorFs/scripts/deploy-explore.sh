#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
profile_root=${DSH_HOME:-${HOME:?HOME is required}/.dsh}/profiles
package_dir=${1:-${CENSORFS_PACKAGE_DIR:-$root/dist/explore}}
bin_dir=${CENSORFS_BIN_DIR:-/usr/local/bin}
credentials_file=${DSH_CREDENTIALS_FILE:-}

[[ -d "$package_dir" ]] || { echo "package directory not found: $package_dir (run scripts/package-explore.sh first)" >&2; exit 2; }
command -v dsh >/dev/null || { echo 'dsh is required' >&2; exit 2; }
command -v pnpm >/dev/null || { echo 'pnpm is required' >&2; exit 2; }
node_major=$(node -p 'process.versions.node.split(".")[0]')
(( node_major >= 22 )) || { echo "Node 22+ is required (found $node_major)" >&2; exit 2; }

plugin=$(find "$package_dir" -maxdepth 1 -type f -name '@censorfs-deepseek-harness-*.tgz' -print -quit)
events=$(find "$package_dir" -maxdepth 1 -type f -name '@censorfs-event-exporter-*.tgz' -print -quit)
[[ -n "$plugin" && -n "$events" ]] || { echo 'both plugin tarballs are required' >&2; exit 2; }

mkdir -p "$bin_dir"
install -m 0755 "$root/integrations/deepseek-harness/bin/dsh-jsonrpc-agent" "$bin_dir/dsh-jsonrpc-agent"

if [[ -n "$credentials_file" ]]; then
  [[ -f "$credentials_file" ]] || { echo "credentials file not found: $credentials_file" >&2; exit 2; }
  mkdir -p "${DSH_HOME:-$HOME/.dsh}"
  install -m 0600 "$credentials_file" "${DSH_HOME:-$HOME/.dsh}/.credentials.yaml"
fi
[[ -f "${DSH_HOME:-$HOME/.dsh}/.credentials.yaml" ]] || {
  echo "missing ${DSH_HOME:-$HOME/.dsh}/.credentials.yaml; provide DSH_CREDENTIALS_FILE (the child cannot rely on DEEPSEEK_API_KEY after sudo env_reset)" >&2
  exit 2
}

install_profile() {
  local profile=$1
  local archive=$2
  local directory="$profile_root/$profile"
  mkdir -p "$directory"
  dsh plugin --profile "$profile" add "$archive"
}

install_profile web "$plugin"
install_profile headless "$events"

cat > "${CENSORFS_ENV_FILE:-$root/censorfs-explore.env}" <<EOF
export DSH_HOME=${DSH_HOME:-$HOME/.dsh}
export CENSORFS_SOCKET=${CENSORFS_SOCKET:-/run/censorfs/control.sock}
export CENSORFS_COMMAND=${CENSORFS_COMMAND:-$bin_dir/censorfs}
export CENSORFS_MOUNTER=${CENSORFS_MOUNTER:-$bin_dir/censorfs-mounter}
export CENSORFS_MOUNTER_BIN=${CENSORFS_MOUNTER_BIN:-$bin_dir/censorfs-mounter}
export DSH_CENSORFS_CHILD_COMMAND=${DSH_CENSORFS_CHILD_COMMAND:-$bin_dir/dsh-jsonrpc-agent}
EOF
chmod 0600 "${CENSORFS_ENV_FILE:-$root/censorfs-explore.env}"

for binary in censorfs censorfsd censorfs-mounter dsh-jsonrpc-agent; do
  command -v "$binary" >/dev/null 2>&1 || [[ -x "$bin_dir/$binary" ]] || { echo "missing deployed executable: $binary" >&2; exit 1; }
done
printf 'DEPLOYED explore plugin; source %s before starting the parent Harness\n' "${CENSORFS_ENV_FILE:-$root/censorfs-explore.env}"
