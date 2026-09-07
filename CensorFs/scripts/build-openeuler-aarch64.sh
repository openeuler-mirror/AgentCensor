#!/usr/bin/env bash
set -euo pipefail

rust_version=1.82.0
install_deps=0
with_tests=0
offline=0

usage() {
  cat <<'EOF'
Build CensorFS natively on an openEuler AArch64 server.

Usage:
  bash scripts/build-openeuler-aarch64.sh [options]

Options:
  --install-deps  Install openEuler build/runtime packages with dnf.
  --with-tests    Run the complete test suite before the release build.
  --offline       Pass --offline to Cargo (requires a populated Cargo cache).
  -h, --help      Show this help.

Environment:
  CENSORFS_TEST_TMPDIR  XFS/ext4 directory used by tests with --with-tests.

Examples:
  bash scripts/build-openeuler-aarch64.sh --install-deps
  CENSORFS_TEST_TMPDIR=/data/censorfs-test-tmp \
    bash scripts/build-openeuler-aarch64.sh --with-tests
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --install-deps) install_deps=1 ;;
    --with-tests) with_tests=1 ;;
    --offline) offline=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

if (( offline == 1 && install_deps == 1 )); then
  echo "--offline cannot be combined with --install-deps" >&2
  exit 2
fi

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

if [[ $(uname -m) != aarch64 ]]; then
  echo "this script requires an AArch64 server; detected $(uname -m)" >&2
  exit 2
fi

if [[ ! -r /etc/os-release ]]; then
  echo "cannot identify the operating system: /etc/os-release is missing" >&2
  exit 2
fi
. /etc/os-release
os_identity="${ID:-} ${ID_LIKE:-} ${NAME:-}"
if [[ ${os_identity,,} != *openeuler* ]]; then
  echo "this script targets openEuler; detected ${PRETTY_NAME:-unknown}" >&2
  exit 2
fi

kernel_release=$(uname -r)
kernel_major=${kernel_release%%.*}
kernel_remainder=${kernel_release#*.}
kernel_minor=${kernel_remainder%%.*}
if ! [[ $kernel_major =~ ^[0-9]+$ && $kernel_minor =~ ^[0-9]+$ ]]; then
  echo "cannot parse kernel version: $kernel_release" >&2
  exit 2
fi
if (( kernel_major < 6 || (kernel_major == 6 && kernel_minor < 6) )); then
  echo "CensorFS requires Linux 6.6 or newer; detected $kernel_release" >&2
  exit 2
fi

if (( install_deps == 1 )); then
  if [[ $(id -u) -eq 0 ]]; then
    dnf install -y \
      gcc gcc-c++ make pkgconf-pkg-config git curl ca-certificates \
      tar gzip findutils util-linux fuse3 jq
  else
    command -v sudo >/dev/null 2>&1 || {
      echo "--install-deps requires root or sudo" >&2
      exit 2
    }
    sudo dnf install -y \
      gcc gcc-c++ make pkgconf-pkg-config git curl ca-certificates \
      tar gzip findutils util-linux fuse3 jq
  fi
fi

required_commands=(gcc make)
if (( offline == 0 )); then
  required_commands+=(curl tar)
fi
for command_name in "${required_commands[@]}"; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "missing dependency: $command_name (rerun with --install-deps)" >&2
    exit 2
  }
done

if (( offline == 1 )); then
  command -v rustup >/dev/null 2>&1 || {
    echo "--offline requires a preinstalled rustup and Rust $rust_version toolchain" >&2
    exit 2
  }
  rustup run "$rust_version" rustc --version >/dev/null 2>&1 || {
    echo "Rust $rust_version is not installed; provision it before using --offline" >&2
    exit 2
  }
else
  if ! command -v rustup >/dev/null 2>&1; then
    rustup_installer=$(mktemp)
    trap 'rm -f -- "$rustup_installer"' EXIT
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$rustup_installer"
    sh "$rustup_installer" -y --profile minimal --default-toolchain "$rust_version"
    cargo_bin_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
    export PATH="$cargo_bin_dir:$PATH"
  fi
  rustup toolchain install "$rust_version" --profile minimal
fi
command -v cargo >/dev/null 2>&1 || {
  echo "cargo is not available after Rust toolchain setup" >&2
  exit 2
}

cargo_flags=(--locked)
if (( offline == 1 )); then
  cargo_flags+=(--offline)
fi

if (( with_tests == 1 )); then
  test_tmp=${CENSORFS_TEST_TMPDIR:-}
  if [[ -z "$test_tmp" ]]; then
    echo "--with-tests requires CENSORFS_TEST_TMPDIR on a local XFS/ext4 filesystem" >&2
    exit 2
  fi
  mkdir -p "$test_tmp"
  test_backing=$(stat -f -c %T "$test_tmp")
  if [[ "$test_backing" != xfs && "$test_backing" != ext2/ext3 ]]; then
    echo "CENSORFS_TEST_TMPDIR must be on local XFS/ext4; found $test_backing" >&2
    exit 2
  fi
  TMPDIR="$test_tmp" cargo +"$rust_version" test \
    --workspace --all-targets "${cargo_flags[@]}"
fi

cargo +"$rust_version" build --workspace --release "${cargo_flags[@]}"
bash scripts/install-censorfs-links.sh "$repo_root/target/release"

echo
echo "CensorFS release build completed."
echo "Artifacts: $repo_root/target/release"
for artifact in censorfs censorfsd censorfsctl censorfs-mounter; do
  if [[ -x "$repo_root/target/release/$artifact" ]]; then
    printf '  %s\n' "$repo_root/target/release/$artifact"
  fi
done
