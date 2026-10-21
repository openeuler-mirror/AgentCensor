#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."
make -C native
mkdir -p target
read -r -a native_cpp <<< "${CPPFLAGS:-}"
read -r -a native_link <<< "${CP_OPENSSL_LIBS:--lssl -lcrypto}"
"${CC:-cc}" "${native_cpp[@]}" -std=c11 -Wall -Wextra -Werror -O2 native/tests/client.c -o target/native-test-client "${native_link[@]}"
cargo test --locked --test tls_native -- --ignored --nocapture
