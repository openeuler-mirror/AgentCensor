#!/usr/bin/env bash
# Censorguard DSH 离线验收套件 (免 root, 对照 ohmyguard C 系列可离线部分)
#
# 验证点:
#   O1  插件 workspace 构建 + 单测全绿 (runtime/bootstrap/host/client)
#   O2  Rust 单测 (common 协议 / daemon 角色 ACL 与域名 / grpc 映射)
#   O3  gRPC 适配器第一线 ACL (无 daemon): Track/Untrack/SetDomainBinding/
#       ApplyPolicy(__base__)/ValidatePolicy(__base__) → PermissionDenied;
#       Status → Unavailable (daemon 不可达错误映射正确)
#   O4  daemon 不可达时 runtime AttachSelfClient 报 connect 错误 (C8 等价,
#       blockOnFailure 语义: DSH 不进 Ready)
#   O5  bundle 静态一致性: cordis.patch.yml 三个插件行 + 8 个 inject 闸门,
#       与 dump-config.mjs 的 REQUIRED 列表一致
#
# 用法: bash scripts/censorguard-dsh-offline.sh   (免 root)

set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
FAILS=0
pass() { echo "  [PASS] $1"; }
fail() { echo "  [FAIL] $1"; FAILS=$((FAILS+1)); }

GRPC_PORT=50099
GRPC_ADDR=127.0.0.1:$GRPC_PORT
TDIR=$(mktemp -d /tmp/censorguard_offline.XXXXXX)
GRPC_LOG=$TDIR/grpc.log
cleanup() {
    [ -n "${GRPC_PID:-}" ] && kill "$GRPC_PID" 2>/dev/null
    rm -rf "$TDIR"
}
trap cleanup EXIT

echo "=== O1: 插件 workspace 构建 + 单测 ==="
if ( cd plugins/dsh-censorguard && pnpm build >/dev/null 2>&1 && pnpm test >/dev/null 2>&1 ); then
    pass "pnpm build + test 全绿"
else
    fail "插件构建/单测失败 (手动跑: cd plugins/dsh-censorguard && pnpm build && pnpm test)"
fi

echo "=== O2: Rust 单测 (common/daemon/grpc) ==="
if cargo test -p censorguard-common -p censorguard-daemon -p censorguard-grpc >/dev/null 2>&1; then
    pass "cargo test 全绿"
else
    fail "cargo test 失败 (手动跑: cargo test -p censorguard-common -p censorguard-daemon -p censorguard-grpc)"
fi

echo "=== O3: gRPC 适配器第一线 ACL (无 daemon) ==="
GRPC_BIN=$ROOT/target/debug/censorguard-grpc
if [ ! -x "$GRPC_BIN" ]; then
    cargo build -p censorguard-grpc >/dev/null 2>&1 || { fail "censorguard-grpc 构建失败"; }
fi
if [ -x "$GRPC_BIN" ]; then
    "$GRPC_BIN" --sock "$TDIR/nonexistent-ui.sock" --ev-sock "$TDIR/nonexistent-events.sock" \
        --listen "$GRPC_ADDR" >"$GRPC_LOG" 2>&1 &
    GRPC_PID=$!
    for _ in $(seq 1 30); do
        grep -q 'READY' "$GRPC_LOG" 2>/dev/null && break
        kill -0 "$GRPC_PID" 2>/dev/null || break
        sleep 0.1
    done

    for METHOD in Track Untrack SetDomainBinding RemovePolicy SetConfig; do
        OUT=$(node scripts/censorguard-grpc-call.mjs "$GRPC_ADDR" "$METHOD" '{}' 2>&1)
        if echo "$OUT" | grep -q 'GRPC_ERROR PERMISSION_DENIED'; then
            pass "$METHOD → PermissionDenied (admin op 第一线拒)"
        else
            fail "$METHOD 未被拒: $OUT"
        fi
    done

    OUT=$(node scripts/censorguard-grpc-call.mjs "$GRPC_ADDR" ApplyPolicy '{"name":"__base__","policyYaml":"rules: []"}' 2>&1)
    if echo "$OUT" | grep -q 'GRPC_ERROR PERMISSION_DENIED'; then
        pass "ApplyPolicy(__base__) → PermissionDenied (适配器前置拒)"
    else
        fail "ApplyPolicy(__base__) 未被拒: $OUT"
    fi

    OUT=$(node scripts/censorguard-grpc-call.mjs "$GRPC_ADDR" ValidatePolicy '{"name":"__base__","policyYaml":"rules: []"}' 2>&1)
    if echo "$OUT" | grep -q 'GRPC_ERROR PERMISSION_DENIED'; then
        pass "ValidatePolicy(__base__) → PermissionDenied (适配器前置拒)"
    else
        fail "ValidatePolicy(__base__) 未被拒: $OUT"
    fi

    OUT=$(node scripts/censorguard-grpc-call.mjs "$GRPC_ADDR" Status '{}' 2>&1)
    if echo "$OUT" | grep -q 'GRPC_ERROR UNAVAILABLE'; then
        pass "Status → Unavailable (daemon 不可达错误映射正确)"
    else
        fail "Status 错误映射不对 (应 Unavailable): $OUT"
    fi
fi

echo "=== O4: daemon 不可达时 AttachSelfClient 报 connect 错误 (C8 等价) ==="
OUT=$(node scripts/test/censorguard-dsh-selfcheck.mjs attach "$TDIR/nonexistent-dsh.sock" censorguard-dsh-default 2>&1)
if [ $? -ne 0 ] && echo "$OUT" | grep -q 'kind=connect'; then
    pass "daemon 不可达 → AttachError kind=connect (blockOnFailure 时 DSH 不进 Ready)"
else
    fail "degraded 路径异常: $OUT"
fi

echo "=== O5: bundle 静态一致性 ==="
PATCH=plugins/dsh-censorguard/bundle/cordis.patch.yml
DUMP=plugins/dsh-censorguard/bundle/bin/dump-config.mjs
O5_OK=1
for line in censorguard-bootstrap censorguard-host censorguard-ui; do
    grep -q "$line" "$PATCH" || { fail "patch 缺插件行 $line"; O5_OK=0; }
done
for target in webserver web-runtime api-gateway cordis-host-runner code-runtime subprocess tools agent-loop; do
    grep -A1 "^- id: $target\$" "$PATCH" | grep -q 'censorguardReady' \
        || { fail "patch 缺 inject 闸门 $target"; O5_OK=0; }
done
# dump-config.mjs 的 REQUIRED 列表与 patch 文件一致 (防两处漂移)
for line in censorguard-bootstrap censorguard-host censorguard-ui \
            webserver web-runtime api-gateway cordis-host-runner code-runtime subprocess tools agent-loop; do
    grep -q "'$line'" "$DUMP" || { fail "dump-config.mjs REQUIRED 缺 $line"; O5_OK=0; }
done
# 官方 subprocess 不得被 disable (整树包裹模式保留官方 entry); 匹配 yaml 键,
# 不匹配注释里的 "disable" 字样
if grep -qE '^\s*disabled:' "$PATCH"; then
    fail "patch 仍含 disabled: 行 (旧设计残留)"; O5_OK=0
fi
[ "$O5_OK" -eq 1 ] && pass "cordis.patch.yml 与 dump-config.mjs 一致, 无 disable 残留"

echo
if [ "$FAILS" -eq 0 ]; then
    echo "全部通过 (O1-O5)"
    exit 0
else
    echo "失败 $FAILS 项"
    exit 1
fi
