#!/usr/bin/env bash
set -euo pipefail
bundle_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
case "${1:-}" in
  verify)
    [[ $# == 1 ]] || { printf 'Usage: ./cpctl verify\n' >&2; exit 2; }
    cd -- "$bundle_root"
    sha256sum --check --quiet SHA256SUMS
    printf '通过：交付包 SHA-256 文件完整性检查。\n'
    ;;
  self-test)
    [[ $# == 1 ]] || { printf 'Usage: ./cpctl self-test\n' >&2; exit 2; }
    "$bundle_root/cpctl" verify
    "$bundle_root/bin/cpctl" doctor --tls --demo
    "$bundle_root/bin/cpctl" demo gateway
    "$bundle_root/bin/cpctl" demo tls
    printf '通过：完整交付包自检（本地真实 NER、网关、TLS；无云调用）。\n'
    ;;
  *) exec "$bundle_root/bin/cpctl" "$@" ;;
esac
