#!/usr/bin/env bash
set -euo pipefail
bundle_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
exec "$bundle_root/bin/censorprivacy" "$@"
