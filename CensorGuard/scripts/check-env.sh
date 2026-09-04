#!/usr/bin/env bash
set -euo pipefail

for tool in rustc cargo clang llvm-strip bpftool pkg-config; do
    command -v "${tool}" >/dev/null
done

test -r /sys/kernel/btf/vmlinux
grep -qw bpf /sys/kernel/security/lsm
pkg-config --exists libbpf

if ! cargo fmt --version >/dev/null 2>&1; then
    echo "warning: cargo-fmt is not installed" >&2
fi
if ! cargo clippy --version >/dev/null 2>&1; then
    echo "warning: cargo-clippy is not installed" >&2
fi

rustc --version
cargo --version
clang --version | head -n 1
bpftool version
pkg-config --modversion libbpf
