#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
model_dir=${CP_DETECTOR_MODEL_DIR:-$PWD/models/bert4ner-base-chinese}
ort_library=${CP_DETECTOR_ORT_LIBRARY:-${ORT_DYLIB_PATH:-}}
node_binary=$(command -v node)
patchelf_binary=${CP_PATCHELF:-patchelf}
output_dir=$PWD/dist
while [[ $# -gt 0 ]]; do
  case "$1" in
    --model-dir) model_dir=${2:?model directory required}; shift 2 ;;
    --ort-library) ort_library=${2:?runtime library required}; shift 2 ;;
    --node) node_binary=${2:?Node binary required}; shift 2 ;;
    --patchelf) patchelf_binary=${2:?patchelf binary required}; shift 2 ;;
    --output-dir) output_dir=${2:?output directory required}; shift 2 ;;
    *) printf 'Usage: bash scripts/package.sh --standalone --ort-library /path/libonnxruntime.so [--model-dir DIR] [--node FILE] [--patchelf FILE] [--output-dir DIR]\n' >&2; exit 2 ;;
  esac
done
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || { printf 'Standalone packaging currently targets Linux x86_64.\n' >&2; exit 1; }
[[ -f $ort_library ]] || { printf 'Provide --ort-library or CP_DETECTOR_ORT_LIBRARY.\n' >&2; exit 1; }
command -v "$patchelf_binary" >/dev/null || { printf 'Install patchelf or provide --patchelf.\n' >&2; exit 1; }
mkdir -p -- "$output_dir"
output_dir=$(realpath -- "$output_dir")
package_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
package_name="censorprivacy-$package_version-linux-x86_64-standalone"
staging=$(mktemp -d "$output_dir/.standalone-XXXXXXXX")
trap 'rm -rf -- "$staging"' EXIT
DESTDIR="$staging" bash scripts/install.sh --prefix /bundle --with-native
"$node_binary" scripts/release/assemble.mjs --directory "$staging/bundle" \
  --model-dir "$model_dir" --ort-library "$ort_library" --node "$node_binary" --patchelf "$patchelf_binary"
mv -- "$staging/bundle" "$staging/$package_name"
tar -C "$staging" -czf "$staging/$package_name.tar.gz" "$package_name"
mv -- "$staging/$package_name.tar.gz" "$output_dir/$package_name.tar.gz"
(cd -- "$output_dir"; sha256sum "$package_name.tar.gz" > "$package_name.tar.gz.sha256")
printf 'Created %s/%s.tar.gz\n' "$output_dir" "$package_name"
