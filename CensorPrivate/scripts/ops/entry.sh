#!/usr/bin/env bash
set -euo pipefail
if [[ -f $CP_REPO_ROOT/BUNDLE.json ]]; then
  CP_BUNDLE_PREFIX=$(cd -- "$CP_DEFAULT_BIN_DIR/.." && pwd -P)
  export CP_BUNDLE_PREFIX
  export PATH="$CP_BUNDLE_PREFIX/runtime/bin:$PATH"
  export CP_INSTALL_PREFIX=$CP_BUNDLE_PREFIX
  export CP_DETECTOR_MODEL_DIR=${CP_DETECTOR_MODEL_DIR:-$CP_REPO_ROOT/models/bert4ner-base-chinese}
  export CP_DETECTOR_ORT_LIBRARY=${CP_DETECTOR_ORT_LIBRARY:-$CP_BUNDLE_PREFIX/lib/censorprivacy/libonnxruntime.so}
  export CURL_CA_BUNDLE=${CURL_CA_BUNDLE:-$CP_REPO_ROOT/ca-bundle.crt}
  export SSL_CERT_FILE=${SSL_CERT_FILE:-$CP_REPO_ROOT/ca-bundle.crt}
fi
export CP_REPO_ROOT CP_DEFAULT_BIN_DIR
exec bash "$CP_REPO_ROOT/scripts/ops/main.sh" "$@"
