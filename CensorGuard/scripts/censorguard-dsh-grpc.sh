#!/usr/bin/env bash
# Censorguard DSH gRPC 委托面验收 (对照 ohmyguard scripts/dsh_grpc_test.sh B1-B9)
#
# 验证点:
#   B1  Status 透传 daemonBootId + hooksHealthy
#   B2  GetPolicy(censorguard-dsh-default) 返回规则
#   B3  GetPolicy(__base__) 成功 (读基线不受限)
#   B4  ValidatePolicy(censorguard-dsh-default, 干净规则) ok
#   B5  ValidatePolicy(__base__) → PermissionDenied (适配器前置拒)
#   B6  ApplyPolicy(__base__) → PermissionDenied (适配器前置拒)
#   B7  ApplyPolicy(白名单外组) → PermissionDenied (daemon ui 角色 ACL 拒)
#   B8  ApplyPolicy(censorguard-dsh-default, 干净规则) 成功
#   B9  Track → PermissionDenied (admin op 第一线拒)
#
# 用法: sudo bash scripts/censorguard-dsh-grpc.sh
# 前置: cargo build (censorguardd, censorguardctl, censorguard-grpc) + 插件 pnpm build
#
# 关键: gRPC 适配器以非 root (nobody) 运行 —— 生产即非特权 systemd unit;
# 若以 root 跑, daemon 端 is_privileged 会绕过组白名单, B7 失去意义。

set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
FAILS=0
pass() { echo "  [PASS] $1"; }
fail() { echo "  [FAIL] $1"; FAILS=$((FAILS+1)); }

if [ "$(id -u)" -ne 0 ]; then
    echo "需要 root: sudo bash $0 (daemon 挂载 eBPF + 下发策略)"; exit 1
fi
DAEMON=$ROOT/target/debug/censorguardd
CTL=$ROOT/target/debug/censorguardctl
GRPC=$ROOT/target/debug/censorguard-grpc
for b in "$DAEMON" "$CTL" "$GRPC"; do
    [ -x "$b" ] || { echo "找不到 $b, 先 cargo build"; exit 1; }
done
command -v setpriv >/dev/null 2>&1 || { echo "缺少 setpriv"; exit 1; }

# sudo 后 root 的 PATH 没有 nvm 的 node, 从调用者的登录 shell 解析
NODE_BIN=$(command -v node 2>/dev/null || true)
if [ -z "$NODE_BIN" ] && [ -n "${SUDO_USER:-}" ]; then
    NODE_BIN=$(sudo -u "$SUDO_USER" bash -lc 'command -v node' 2>/dev/null || true)
fi
[ -n "$NODE_BIN" ] || { echo "找不到 node (root PATH 无 nvm, 调用者登录 shell 也没有)"; exit 1; }

TDIR=$(mktemp -d /tmp/censorguard_grpc.XXXXXX)
chmod 0755 "$TDIR"
CTL_SOCK=$TDIR/ctl.sock
EV_SOCK=$TDIR/events.sock
UI_SOCK=$TDIR/ui.sock   # sibling 自动推导
DAEMON_LOG=$TDIR/daemon.log
GRPC_LOG=$TDIR/grpc.log
GRPC_PORT=50098
GRPC_ADDR=127.0.0.1:$GRPC_PORT
GRPC_CALL="$NODE_BIN $ROOT/scripts/censorguard-grpc-call.mjs $GRPC_ADDR"

cleanup() {
    [ -n "${GRPC_PID:-}" ] && kill "$GRPC_PID" 2>/dev/null
    [ -n "${DAEMON_PID:-}" ] && kill "$DAEMON_PID" 2>/dev/null
    rm -rf "$TDIR"
}
trap cleanup EXIT

"$DAEMON" --ctl-sock "$CTL_SOCK" --event-sock "$EV_SOCK" --state-dir "$TDIR/state" \
    >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 100); do
    grep -q 'required hooks attached' "$DAEMON_LOG" 2>/dev/null && break
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "daemon 早退:"; cat "$DAEMON_LOG"; exit 1; }
    sleep 0.1
done
grep -q 'required hooks attached' "$DAEMON_LOG" || { echo "daemon 未 READY:"; cat "$DAEMON_LOG"; exit 1; }

# 非特权运行适配器 (生产姿态), ui.sock/events.sock 是 0666 可连
setpriv --reuid 65534 --regid 65534 --clear-groups \
    "$GRPC" --sock "$UI_SOCK" --ev-sock "$EV_SOCK" --listen "$GRPC_ADDR" \
    >"$GRPC_LOG" 2>&1 &
GRPC_PID=$!
for _ in $(seq 1 50); do
    grep -q 'READY' "$GRPC_LOG" 2>/dev/null && break
    kill -0 "$GRPC_PID" 2>/dev/null || { echo "gRPC 早退:"; cat "$GRPC_LOG"; exit 1; }
    sleep 0.1
done

"$CTL" --sock "$CTL_SOCK" policy apply --name censorguard-dsh-default \
    --file config/policy.dsh-default.yaml >/dev/null 2>&1 \
    && pass "准备: 下发 censorguard-dsh-default" || fail "准备: 策略下发失败"

VALID_YAML='rules:\n  - file deny+audit /tmp/censorguard_grpc_test [write,delete]'

echo "=== B1: Status 透传 daemonBootId + hooksHealthy ==="
OUT=$($GRPC_CALL Status '{}')
if echo "$OUT" | grep -qP '"daemonBootId": *".+"' && echo "$OUT" | grep -q '"hooksHealthy": *true'; then
    pass "Status 回执完整"
else
    fail "Status 异常: $OUT"
fi

echo "=== B2: GetPolicy(censorguard-dsh-default) 返回规则 ==="
OUT=$($GRPC_CALL GetPolicy '{"name":"censorguard-dsh-default"}')
if echo "$OUT" | grep -q '"rules": *\[' && echo "$OUT" | grep -q 'censorguard-dsh-default'; then
    pass "GetPolicy 返回规则"
else
    fail "GetPolicy 异常: $OUT"
fi

echo "=== B3: GetPolicy(__base__) 成功 ==="
OUT=$($GRPC_CALL GetPolicy '{"name":"__base__"}')
if echo "$OUT" | grep -q '"name": *"__base__"'; then
    pass "GetPolicy(__base__) 成功"
else
    fail "GetPolicy(__base__) 异常: $OUT"
fi

echo "=== B4: ValidatePolicy(censorguard-dsh-default, 干净规则) ok ==="
OUT=$($GRPC_CALL ValidatePolicy "{\"name\":\"censorguard-dsh-default\",\"policyYaml\":\"$VALID_YAML\"}")
if echo "$OUT" | grep -q '"ok": *true'; then
    pass "ValidatePolicy 通过"
else
    fail "ValidatePolicy 异常: $OUT"
fi

echo "=== B5: ValidatePolicy(__base__) → PermissionDenied ==="
OUT=$($GRPC_CALL ValidatePolicy '{"name":"__base__","policyYaml":"rules: []"}' 2>&1)
if echo "$OUT" | grep -q 'GRPC_ERROR PERMISSION_DENIED'; then
    pass "ValidatePolicy(__base__) 被拒"
else
    fail "ValidatePolicy(__base__) 未被拒: $OUT"
fi

echo "=== B6: ApplyPolicy(__base__) → PermissionDenied ==="
OUT=$($GRPC_CALL ApplyPolicy '{"name":"__base__","policyYaml":"rules: []"}' 2>&1)
if echo "$OUT" | grep -q 'GRPC_ERROR PERMISSION_DENIED'; then
    pass "ApplyPolicy(__base__) 被拒"
else
    fail "ApplyPolicy(__base__) 未被拒: $OUT"
fi

echo "=== B7: ApplyPolicy(白名单外组) → PermissionDenied (daemon ui ACL) ==="
OUT=$($GRPC_CALL ApplyPolicy '{"name":"standard-dev","policyYaml":"rules: []"}' 2>&1)
if echo "$OUT" | grep -q 'GRPC_ERROR PERMISSION_DENIED' && echo "$OUT" | grep -q 'not editable'; then
    pass "白名单外组被 daemon ui 角色 ACL 拒"
else
    fail "白名单外组未被拒: $OUT"
fi

echo "=== B8: ApplyPolicy(censorguard-dsh-default, 干净规则) 成功 ==="
OUT=$($GRPC_CALL ApplyPolicy "{\"name\":\"censorguard-dsh-default\",\"policyYaml\":\"$VALID_YAML\"}")
if echo "$OUT" | grep -q '"name": *"censorguard-dsh-default"'; then
    pass "ApplyPolicy 成功"
    # 恢复仓库里的完整策略 (B8 把它整组替换成单条规则了)
    "$CTL" --sock "$CTL_SOCK" policy apply --name censorguard-dsh-default \
        --file config/policy.dsh-default.yaml >/dev/null 2>&1
else
    fail "ApplyPolicy 异常: $OUT"
fi

echo "=== B9: Track → PermissionDenied (admin op) ==="
OUT=$($GRPC_CALL Track '{"pid":1,"seed":true}' 2>&1)
if echo "$OUT" | grep -q 'GRPC_ERROR PERMISSION_DENIED'; then
    pass "Track 被拒"
else
    fail "Track 未被拒: $OUT"
fi

echo
if [ "$FAILS" -eq 0 ]; then
    echo "全部通过 (B1-B9)"
    exit 0
else
    echo "失败 $FAILS 项"
    echo "--- daemon 日志 ---"; tail -20 "$DAEMON_LOG"
    echo "--- gRPC 日志 ---"; tail -20 "$GRPC_LOG"
    exit 1
fi
