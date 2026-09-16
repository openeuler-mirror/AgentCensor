#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
package_dir=${1:-${CENSORPIVOT_PACKAGE_DIR:-$root/dist/dsh-censorpivot}}
reuse_packages=false
if [[ $# -gt 0 || -n ${CENSORPIVOT_PACKAGE_DIR:-} ]]; then
  reuse_packages=true
fi
dsh_home=${DSH_HOME:-${HOME:?HOME is required}/.dsh}
bin_dir=${AGENTCENSOR_BIN_DIR:-$HOME/.local/bin}
dsh_command=()
pnpm_store_args=()

fail() {
  printf '[ERROR] %s\n' "$1" >&2
  [[ $# -lt 2 ]] || printf '[HINT] %s\n' "$2" >&2
  exit 2
}

is_dsh_source_root() {
  local candidate=$1
  [[ -f "$candidate/package.json" ]] && node -e \
    'const p=require(process.argv[1]); process.exit(p.scripts?.dsh ? 0 : 1)' \
    "$candidate/package.json"
}

use_dsh_source_root() {
  local candidate=$1
  is_dsh_source_root "$candidate" || fail \
    "not a DeepSeek Harness source tree: $candidate" \
    "set DSH_ROOT to a directory whose package.json defines the dsh script"
  dsh_command=(pnpm --dir "$candidate" dsh)
  printf '[INFO] using DeepSeek Harness source tree: %s\n' "$candidate"
}

resolve_dsh_command() {
  local workspace_parent
  workspace_parent=$(cd "$root/../.." && pwd)
  if [[ -n ${DSH_BIN:-} ]]; then
    if [[ -d "$DSH_BIN" ]]; then
      use_dsh_source_root "$DSH_BIN"
    elif [[ -x "$DSH_BIN" ]]; then
      dsh_command=("$DSH_BIN")
    elif command -v "$DSH_BIN" >/dev/null 2>&1; then
      dsh_command=("$DSH_BIN")
    else
      fail "DSH_BIN is not an executable command: $DSH_BIN" \
        "use DSH_BIN=/path/to/dsh or DSH_ROOT=/path/to/deepseek-harness (without spaces around =)"
    fi
  elif [[ -n ${DSH_ROOT:-} ]]; then
    use_dsh_source_root "$DSH_ROOT"
  elif command -v dsh >/dev/null 2>&1; then
    dsh_command=(dsh)
  elif is_dsh_source_root "$PWD"; then
    use_dsh_source_root "$PWD"
  elif is_dsh_source_root "$workspace_parent/deepseek-harness"; then
    use_dsh_source_root "$workspace_parent/deepseek-harness"
  else
    fail "dsh command and DeepSeek Harness source tree were not found" \
      "install dsh, or run with DSH_ROOT=/absolute/path/to/deepseek-harness"
  fi
}

for command in node pnpm; do
  command -v "$command" >/dev/null || fail "$command is required"
done
resolve_dsh_command
mkdir -p "$dsh_home"
if [[ -n ${DSH_PNPM_STORE_DIR:-} ]]; then
  mkdir -p "$DSH_PNPM_STORE_DIR"
  pnpm_store_args=(--store-dir "$DSH_PNPM_STORE_DIR")
fi
node_major=$(node -p 'process.versions.node.split(".")[0]')
(( node_major >= 22 )) || fail "Node 22+ is required (found $node_major)"

required=(
  censorpivot-web.tgz censorpivot-headless.tgz
  censorfs-web.tgz censorfs-headless.tgz censorguard-web.tgz
  censorscope-host.tgz censorscope-ui.tgz censorscope-session-proxy.tgz
)
missing=false
for archive in "${required[@]}"; do
  [[ -f "$package_dir/$archive" ]] || missing=true
done
if [[ "$missing" == true || "$reuse_packages" == false ]]; then
  "$root/scripts/package-dsh-censorpivot.sh" "$package_dir"
fi

initialize_profile() {
  local profile=$1
  "${dsh_command[@]}" plugin --profile "$profile" list --depth 0 >/dev/null
}

install_runtime() {
  local profile=$1
  shift
  local profile_dir="$dsh_home/profiles/$profile"
  initialize_profile "$profile"
  pnpm --dir "$profile_dir" "${pnpm_store_args[@]}" \
    add --save-exact --force --ignore-scripts "$@"
}

install_runtime web \
  "$package_dir/censorfs-web.tgz" \
  "$package_dir/censorguard-web.tgz" \
  "$package_dir/censorscope-host.tgz" \
  "$package_dir/censorscope-ui.tgz" \
  "$package_dir/censorscope-session-proxy.tgz"
"${dsh_command[@]}" plugin --profile web add "${pnpm_store_args[@]}" \
  --force --ignore-scripts "$package_dir/censorpivot-web.tgz"

install_runtime headless \
  "$package_dir/censorfs-headless.tgz" \
  "$package_dir/censorscope-host.tgz"
"${dsh_command[@]}" plugin --profile headless add "${pnpm_store_args[@]}" \
  --force --ignore-scripts "$package_dir/censorpivot-headless.tgz"

# CensorFS's full child-Harness path needs this adapter on PATH. The three
# native module daemons/binaries remain owned by their normal system install.
censorfs_root=${CENSORFS_ROOT:-$(cd "$root/../CensorFs" && pwd)}
mkdir -p "$bin_dir"
install -m 0755 "$censorfs_root/integrations/deepseek-harness/bin/dsh-jsonrpc-agent" \
  "$bin_dir/dsh-jsonrpc-agent"

node - "$dsh_home" <<'NODE'
const fs = require('node:fs')
const path = require('node:path')
const home = process.argv[2]
const expected = {
  web: '@agentcensor/censorpivot',
  headless: '@agentcensor/censorpivot-headless',
}
const retired = new Set([
  '@censorfs/deepseek-harness', '@censorfs/event-exporter', '@censorguard/dsh',
  'censorscope-host', 'censorscope-ui', 'agentcensor-session-proxy',
])
for (const [profile, active] of Object.entries(expected)) {
  const file = path.join(home, 'profiles', profile, 'package.json')
  const pkg = JSON.parse(fs.readFileSync(file, 'utf8'))
  const bundles = pkg.dsh?.profile?.bundles ?? []
  if (!bundles.includes(active)) throw new Error(`${profile}: missing ${active} bundle`)
  const duplicate = bundles.find(name => retired.has(name))
  if (duplicate) throw new Error(`${profile}: old component bundle still active: ${duplicate}`)
}
NODE

printf 'CensorPivot DSH plugin installed (web + internal headless companion).\n'
if [[ ":$PATH:" != *":$bin_dir:"* ]]; then
  printf 'Add %s to PATH before starting dsh.\n' "$bin_dir"
fi
