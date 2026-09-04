#!/usr/bin/env bash
# Censorguard DSH attach 自助面验收 (对照 ohmyguard scripts/dsh_attach_self_test.sh A1-A9)
#
# 验证点:
#   A1  ctl.sock (root) 下发 censorguard-dsh-default 策略组
#   A2  非 root 经 dsh.sock attach_self 成功, 回执带 daemonBootId + hooksHealthy
#   A3  两个并发 attach (不同 uid) 得到不同 Domain
#   A4  dsh.sock ACL: 非自助 op (health) 被拒 "not allowed on the Dsh socket"
#   A5  attach_self 忽略伪造 pid (params.pid=1, 域名仍取 SO_PEERCRED 真实 tgid)
#   A6  status_self: 新进程不属于任何 Domain (ok=true, domains 空)
#   A7  attach_self 在 ctl.sock (Admin 角色) 被 ACL 拒
#   A8  白名单外组 (__base__) attach_self 被拒 "not in the DSH self-serve whitelist"
#   A9  Domain 名格式 dsh:<uid>:<tgid>-<starttime>
#
# 用法: sudo bash scripts/test/censorguard-dsh-attach-self.sh
# 前置: cargo build (target/debug/censorguardd, censorguardctl) + 插件 pnpm build

set -u
cd "$(dirname "$0")/../.."
ROOT=$PWD
FAILS=0
pass() { echo "  [PASS] $1"; }
fail() { echo "  [FAIL] $1"; FAILS=$((FAILS+1)); }

if [ "$(id -u)" -ne 0 ]; then
    echo "需要 root: sudo bash $0 (daemon 挂载 eBPF + 下发策略)"; exit 1
fi
DAEMON=$ROOT/target/debug/censorguardd
CTL=$ROOT/target/debug/censorguardctl
SELFCHECK=$ROOT/scripts/test/censorguard-dsh-selfcheck.mjs
for b in "$DAEMON" "$CTL"; do
    [ -x "$b" ] || { echo "找不到 $b, 先 cargo build"; exit 1; }
done
[ -f "$ROOT/plugins/dsh-censorguard/packages/runtime/dist/index.js" ] \
    || { echo "runtime 未构建: cd plugins/dsh-censorguard && pnpm build"; exit 1; }
command -v setpriv >/dev/null 2>&1 || { echo "缺少 setpriv"; exit 1; }

# sudo 后 root 的 PATH 没有 nvm 的 node, 从调用者的登录 shell 解析
NODE_BIN=$(command -v node 2>/dev/null || true)
if [ -z "$NODE_BIN" ] && [ -n "${SUDO_USER:-}" ]; then
    NODE_BIN=$(sudo -u "$SUDO_USER" bash -lc 'command -v node' 2>/dev/null || true)
fi
[ -n "$NODE_BIN" ] || { echo "找不到 node (root PATH 无 nvm, 调用者登录 shell 也没有)"; exit 1; }

# nobody 用户经 nvm 路径无权访问 node 与源码, 复制到临时目录
TDIR=$(mktemp -d /tmp/censorguard_attach.XXXXXX)
chmod 0755 "$TDIR"
NODE_TMP=$TDIR/node
cp "$NODE_BIN" "$NODE_TMP" && chmod 0755 "$NODE_TMP"
RT_TMP=$TDIR/runtime-dist
mkdir -p "$RT_TMP"
cp -r "$ROOT/plugins/dsh-censorguard/packages/runtime/dist/." "$RT_TMP/"
cp "$SELFCHECK" "$TDIR/selfcheck.mjs"
chmod -R 0755 "$RT_TMP"

CTL_SOCK=$TDIR/ctl.sock
DSH_SOCK=$TDIR/dsh.sock   # sibling 自动推导 (--ctl-sock 非 /run 时)
EV_SOCK=$TDIR/events.sock
DAEMON_LOG=$TDIR/daemon.log

cleanup() {
    [ -n "${DAEMON_PID:-}" ] && kill "$DAEMON_PID" 2>/dev/null
    rm -rf "$TDIR"
}
trap cleanup EXIT

# selfcheck.mjs 里 import 路径是相对 scripts/test/ 的, 临时副本改为相对 ./runtime-dist
sed -i "s|'../../plugins/dsh-censorguard/packages/runtime/dist/index.js'|'./runtime-dist/index.js'|" \
    "$TDIR/selfcheck.mjs"

"$DAEMON" --ctl-sock "$CTL_SOCK" --event-sock "$EV_SOCK" --state-dir "$TDIR/state" \
    >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 100); do
    grep -q 'required hooks attached' "$DAEMON_LOG" 2>/dev/null && break
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "daemon 早退:"; cat "$DAEMON_LOG"; exit 1; }
    sleep 0.1
done
grep -q 'required hooks attached' "$DAEMON_LOG" || { echo "daemon 未 READY:"; cat "$DAEMON_LOG"; exit 1; }

run_as_nobody() { setpriv --reuid 65534 --regid 65534 --clear-groups "$@"; }

echo "=== A1: ctl.sock 下发 censorguard-dsh-default ==="
if "$CTL" --sock "$CTL_SOCK" policy apply --name censorguard-dsh-default \
        --file config/policy.dsh-default.yaml >/dev/null 2>&1; then
    pass "策略组下发成功"
else
    fail "策略组下发失败"
fi

echo "=== A2: 非 root attach_self 成功 ==="
A2_OUT=$TDIR/a2.out
if run_as_nobody "$NODE_TMP" "$TDIR/selfcheck.mjs" attach "$DSH_SOCK" censorguard-dsh-default >"$A2_OUT" 2>&1; then
    if grep -q 'daemonBootId=.\+' "$A2_OUT" && grep -q 'hooksHealthy=true' "$A2_OUT"; then
        pass "attach 成功, 回执完整 ($(grep -oP '域 "\K[^"]+' "$A2_OUT"))"
    else
        fail "回执缺 daemonBootId/hooksHealthy"; cat "$A2_OUT"
    fi
else
    fail "非 root attach_self 失败"; cat "$A2_OUT"
fi

echo "=== A3: 并发 attach 不同 uid 得到不同 Domain ==="
A3A=$TDIR/a3a.out; A3B=$TDIR/a3b.out
( setpriv --reuid 65534 --regid 65534 --clear-groups "$NODE_TMP" "$TDIR/selfcheck.mjs" attach "$DSH_SOCK" censorguard-dsh-default >"$A3A" 2>&1 ) &
P1=$!
( setpriv --reuid 65533 --regid 65533 --clear-groups "$NODE_TMP" "$TDIR/selfcheck.mjs" attach "$DSH_SOCK" censorguard-dsh-default >"$A3B" 2>&1 ) &
P2=$!
wait $P1; wait $P2
DOM_A=$(grep -oP '域 "\K[^"]+' "$A3A" | head -1)
DOM_B=$(grep -oP '域 "\K[^"]+' "$A3B" | head -1)
if [ -n "$DOM_A" ] && [ -n "$DOM_B" ] && [ "$DOM_A" != "$DOM_B" ]; then
    pass "不同 Domain: $DOM_A vs $DOM_B"
else
    fail "未得到不同 Domain (a=$DOM_A b=$DOM_B)"; cat "$A3A" "$A3B"
fi

echo "=== A4: dsh.sock 拒绝非自助 op (health) ==="
OUT=$(run_as_nobody "$NODE_TMP" "$TDIR/selfcheck.mjs" raw "$DSH_SOCK" health '{}' 2>&1)
if echo "$OUT" | grep -q 'not allowed on the Dsh socket'; then
    pass "health 在 dsh.sock 被 ACL 拒"
else
    fail "dsh.sock 未拒绝 health: $OUT"
fi

echo "=== A5: attach_self 忽略伪造 pid (params.pid=1) ==="
OUT=$(run_as_nobody "$NODE_TMP" "$TDIR/selfcheck.mjs" raw "$DSH_SOCK" attach_self \
    '{"policy_group":"censorguard-dsh-default","pid":1,"seed":false}' 2>&1)
# 域名必须是 dsh-65534-<真实tgid>-..., 不能含 pid 1 的 tgid
if echo "$OUT" | grep -q '"ok": *true' && echo "$OUT" | grep -qP '"name": *"dsh-65534-\d+-\d+"'; then
    pass "pid 取 SO_PEERCRED, 伪造 pid=1 无效 ($(echo "$OUT" | grep -oP '"name": *"\K[^"]+'))"
else
    fail "attach_self 伪造 pid 处理异常: $OUT"
fi

echo "=== A6: status_self 新进程不属于任何 Domain ==="
OUT=$(run_as_nobody "$NODE_TMP" "$TDIR/selfcheck.mjs" raw "$DSH_SOCK" status_self '{}' 2>&1)
if echo "$OUT" | grep -q '"ok": *true' && echo "$OUT" | grep -qP '"domains": *\[\]'; then
    pass "status_self 返回空域列表 (语义正确: 只有 attach 调用者自身在域内)"
else
    fail "status_self 输出异常: $OUT"
fi

echo "=== A7: attach_self 在 ctl.sock (Admin) 被拒 ==="
OUT=$("$NODE_TMP" "$TDIR/selfcheck.mjs" raw "$CTL_SOCK" attach_self \
    '{"policy_group":"censorguard-dsh-default"}' 2>&1)
if echo "$OUT" | grep -q 'not allowed on the Admin socket'; then
    pass "Admin 角色拒绝 attach_self"
else
    fail "Admin 未拒绝 attach_self: $OUT"
fi

echo "=== A8: 白名单外组 (__base__) attach_self 被拒 ==="
OUT=$(run_as_nobody "$NODE_TMP" "$TDIR/selfcheck.mjs" raw "$DSH_SOCK" attach_self \
    '{"policy_group":"__base__","seed":false}' 2>&1)
if echo "$OUT" | grep -q 'not in the DSH self-serve whitelist'; then
    pass "__base__ 被白名单拒"
else
    fail "__base__ 拒绝原因不对: $OUT"
fi

echo "=== A9: Domain 名格式 ==="
if [ -n "$DOM_A" ] && echo "$DOM_A" | grep -qP '^dsh-65534-\d+-\d+$'; then
    pass "格式 dsh-<uid>-<tgid>-<starttime> 正确 ($DOM_A)"
else
    fail "Domain 名格式不对 ($DOM_A)"
fi

echo
if [ "$FAILS" -eq 0 ]; then
    echo "全部通过 (A1-A9)"
    exit 0
else
    echo "失败 $FAILS 项"
    echo "--- daemon 日志 ---"; tail -20 "$DAEMON_LOG"
    exit 1
fi
