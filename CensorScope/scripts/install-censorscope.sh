#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DSH_HOME="${DSH_HOME:-$HOME/.dsh}"
mode="${1:-all}"
profiles=("web" "headless")
install_ui="0"
case "$mode" in
  host) install_ui="0" ;;
  all) install_ui="1" ;;
  *) printf 'usage: %s [all|host]\n' "$0" >&2; exit 2 ;;
esac
cache="${PNPM_STORE_DIR:-}"
cache_args=()
if [ -n "$cache" ]; then
  mkdir -p "$cache" "${PNPM_CACHE_DIR:-$cache/cache}"
  cache_args=(--store-dir="$cache" --cache-dir="${PNPM_CACHE_DIR:-$cache/cache}")
fi
pack() {
  local pkg="$1"
  local file
  file="$(cd "$root/plugins/$pkg" && npm pack --pack-destination . --cache "${NPM_CACHE_DIR:-$root/.cache/npm}" 2>/dev/null | tail -n 1)"
  printf '%s\n' "$root/plugins/$pkg/$file"
}
ensure() {
  local pkg="$1" profile="$2"
  local file="$DSH_HOME/profiles/$profile/package.json"
  if [ ! -f "$file" ]; then
    dsh --profile "$profile" --help >/dev/null 2>&1 || true
  fi
  if [ ! -f "$file" ]; then
    printf 'profile %s missing at %s (run: dsh --profile %s --help)\n' "$profile" "$DSH_HOME/profiles/$profile" "$profile" >&2
    exit 1
  fi
  local local_ver inst_ver
  local_ver="$(node -p "require('$root/plugins/$pkg/package.json').version")"
  inst_ver=""
  if [ -f "$DSH_HOME/profiles/$profile/node_modules/$pkg/package.json" ]; then
    inst_ver="$(node -p "require('$DSH_HOME/profiles/$profile/node_modules/$pkg/package.json').version")"
  elif [ -f "$DSH_HOME/profiles/node_modules/$pkg/package.json" ]; then
    inst_ver="$(node -p "require('$DSH_HOME/profiles/node_modules/$pkg/package.json').version")"
  fi
  local present
  present="$(node -e "const d=require(process.argv[1]);const p=process.argv[2];const b=d.dsh&&d.dsh.profile&&d.dsh.profile.bundles||[];const x=(d.dependencies||{})[p]!==undefined;process.stdout.write(String(b.includes(p)||x))" "$file" "$pkg")"
  if [ "$present" = "true" ] && [ "$inst_ver" = "$local_ver" ]; then
    printf '%s %s installed in profile %s\n' "$pkg" "$local_ver" "$profile"
    return 0
  fi
  if [ -n "$inst_ver" ]; then
    printf 'refreshing %s %s -> %s in profile %s\n' "$pkg" "$inst_ver" "$local_ver" "$profile"
  else
    printf 'installing %s %s into profile %s\n' "$pkg" "$local_ver" "$profile"
  fi
  local tgz
  tgz=$(pack "$pkg")
  node -e "const fs=require('fs');const f=process.argv[1];const p=process.argv[2];const t=process.argv[3];const d=JSON.parse(fs.readFileSync(f,'utf8'));d.dependencies=d.dependencies||{};d.dependencies[p]=t;const b=d.dsh&&d.dsh.profile&&d.dsh.profile.bundles||[];if(!b.includes(p))b.push(p);d.dsh.profile.bundles=b;fs.writeFileSync(f,JSON.stringify(d,null,2)+'\n')" "$file" "$pkg" "file:$tgz"
  rm -rf "$DSH_HOME/profiles/$profile/node_modules/$pkg" "$DSH_HOME/profiles/node_modules/$pkg"
  dsh plugin --profile "$profile" add "${cache_args[@]}" "$tgz"
}
for profile in "${profiles[@]}"; do
  ensure censorscope-host "$profile"
done
ensure agentcensor-session-proxy web
if [ "$install_ui" = "1" ]; then
  ensure censorscope-ui web
fi
