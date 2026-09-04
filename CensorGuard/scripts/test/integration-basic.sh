#!/usr/bin/env bash
# 基础验收测试 (一行式策略 + 运行时开关模型)
# 风格对齐 ohmyguard-link/scripts/pidtree_p1_test.sh
#
# 用法: sudo bash scripts/integration-basic.sh
# 前置: cargo build --workspace && make bpf
#
# 覆盖:
#   T1  文件读拦截 (基线规则) + audit DENY 事件
#   T2  文件写拦截 + 内容未被破坏
#   T3  无规则文件放行 (不误伤)
#   T4  exec 拦截 (组规则)
#   T5  组隔离: 未绑定组规则的域放行同一命令
#   T6  嵌套子孙 exec 仍被拦 (进程树继承)
#   T7  运行时开关: set --audit-exec on 后 ALLOW 事件可见 (替代旧 allow_sample_rate)
#   T8  运行时开关: set --enable-exec off 后 exec 维 fail-open
#   T9  整份热更新: reload --file 后新规则生效
#   T10 组级热更新: policy apply 新组 + bind 换绑生效 (整组替换语义)
#   T11 旧格式/非法 YAML 被拒且旧策略保留
#   T12 attach --pid: 已运行进程纳管后新子孙受控
#   T13 根退出后追踪集清理 (有 bpftool 才跑)
#   T14 daemon 未起时 ctl 非 0 退出
#
# socket/状态目录全部隔离在 $TDIR, 不碰 /run 与生产 state。

set -u
cd "$(dirname "$0")/../.."

DAEMON=target/debug/censorguardd
CTL=target/debug/censorguardctl
AUDIT=target/debug/censorguard-audit
FAILS=0
pass() { echo "  [PASS] $1"; }
fail() { echo "  [FAIL] $1"; FAILS=$((FAILS+1)); }
skip() { echo "  [SKIP] $1"; }

if [ "$(id -u)" -ne 0 ]; then
    echo "需要 root: sudo bash $0"; exit 1
fi
for b in "$DAEMON" "$CTL" "$AUDIT"; do
    [ -x "$b" ] || { echo "找不到 $b, 先 cargo build --workspace && make bpf"; exit 1; }
done
[ -f bpf/enforce.bpf.o ] || { echo "找不到 bpf/enforce.bpf.o, 先 make bpf"; exit 1; }
HAVE_BPFTOOL=1; command -v bpftool >/dev/null || { HAVE_BPFTOOL=0; echo "[!] 无 bpftool, T13 降级为 skip"; }

TDIR=$(mktemp -d /tmp/agsec-basic.XXXXXX)
OUT=$TDIR/out; mkdir -p "$OUT"
VICTIM=$TDIR/victim.txt
VICTIM2=$TDIR/victim2.txt
ALLOWED=$TDIR/allowed.txt
echo "secret" > "$VICTIM"
echo "secret2" > "$VICTIM2"
echo "hello"  > "$ALLOWED"
export VICTIM VICTIM2 ALLOWED OUT

IDBIN=$(command -v id)
WHOAMIBIN=$(command -v whoami)

# ---- 一行式策略: 基线拦 VICTIM; ops 组拦 id; web 域绑 ops, plain 域仅基线 ----
POL=$TDIR/policy.yaml
cat > "$POL" <<YAML
rules:
  - file deny+audit ${VICTIM}
groups:
  ops:
    rules:
      - exec deny ${IDBIN}
domains:
  - name: web
    group: ops
  - name: plain
YAML

CTL_SOCK=$TDIR/ctl.sock
EV_SOCK=$TDIR/events.sock
DAEMON_LOG=$TDIR/daemon.log
AUDIT_LOG=$TDIR/audit.log

"$DAEMON" \
    --config "$POL" \
    --bpf-object bpf/enforce.bpf.o \
    --ctl-sock "$CTL_SOCK" \
    --event-sock "$EV_SOCK" \
    --duration 120 >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 100); do
    grep -q '\[READY\]' "$DAEMON_LOG" 2>/dev/null && break
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "daemon 早退, 日志:"; cat "$DAEMON_LOG"; exit 1; }
    sleep 0.1
done
grep -q '\[READY\]' "$DAEMON_LOG" || { echo "daemon 未 READY, 日志:"; cat "$DAEMON_LOG"; exit 1; }

"$AUDIT" launch --socket "$EV_SOCK" >"$AUDIT_LOG" 2>&1 &
AUDIT_PID=$!
for _ in $(seq 1 50); do grep -q '^KIND' "$AUDIT_LOG" 2>/dev/null && break; sleep 0.1; done
grep -q '^KIND' "$AUDIT_LOG" || { echo "audit 未连上事件面, 日志:"; cat "$AUDIT_LOG"; exit 1; }

cleanup() {
    kill "$AUDIT_PID" "$DAEMON_PID" 2>/dev/null
    jobs -p | xargs -r kill 2>/dev/null
}
trap cleanup EXIT

echo "==  fixtures: $TDIR"
echo "==  policy:   $POL"
echo "==  daemon:   pid=$DAEMON_PID log=$DAEMON_LOG"
echo "==  audit:    pid=$AUDIT_PID log=$AUDIT_LOG"

# 在指定域里跑一次命令, 返回真实 rc (spawn 以子进程退出码为准)
run_in_domain() {  # $1=域 $2..=命令
    local dom=$1; shift
    "$CTL" --socket "$CTL_SOCK" spawn --domain "$dom" -- "$@" >/dev/null 2>&1
    echo $?
}

# 等待 audit 日志出现指定模式 (事件流是异步的)
wait_audit() {  # $1=grep 模式
    for _ in $(seq 1 50); do
        grep -q "$1" "$AUDIT_LOG" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}

echo
echo "== T1/T2/T3: 文件拦截基线 =="
rc=$(run_in_domain web cat "$VICTIM")
[ "$rc" != "0" ] && pass "T1 读受保护文件被拒 (web 域)" || fail "T1 读未被拦 (rc=$rc)"
rc=$(run_in_domain web bash -c "echo x >> '$VICTIM'")
[ "$rc" != "0" ] && [ "$(cat "$VICTIM")" = "secret" ] \
    && pass "T2 写受保护文件被拒且内容未变" || fail "T2 写拦截失效 (rc=$rc, content=$(cat "$VICTIM"))"
rc=$(run_in_domain web cat "$ALLOWED")
[ "$rc" = "0" ] && pass "T3 无规则文件放行" || fail "T3 误伤无规则文件"
wait_audit 'DENY' && pass "T1 audit 日志有 DENY 事件" || fail "T1 audit 日志无 DENY 事件"

echo
echo "== T4/T5/T6: exec 组规则与继承 =="
rc=$(run_in_domain web "$IDBIN")
[ "$rc" != "0" ] && pass "T4 exec 拦截生效 (web 域禁 id)" || fail "T4 id 未被拦 (rc=$rc)"
rc=$(run_in_domain plain "$IDBIN" -u)
[ "$rc" = "0" ] && pass "T5 组隔离: plain 域放行 id (仅基线)" || fail "T5 plain 域被误伤"
rc=$(run_in_domain web bash -c 'bash -c "cat \"$0\""' "$VICTIM")
[ "$rc" != "0" ] && pass "T6 嵌套子孙 exec 仍被拦" || fail "T6 嵌套子孙脱出 scope"
wait_audit 'EXEC.*DENY.*web' && pass "T4 audit 日志有 EXEC DENY 事件" || fail "T4 audit 无 EXEC DENY"

echo
echo "== T7/T8: 运行时开关 (ctl set) =="
"$CTL" --socket "$CTL_SOCK" set --audit-exec on >/dev/null 2>&1 \
    || fail "T7 set --audit-exec on 调用失败"
"$CTL" --socket "$CTL_SOCK" status 2>/dev/null | grep -q '"audit_exec": true' \
    && pass "T7 status 显示 audit_exec=true" || fail "T7 status 未见 audit_exec=true"
rc=$(run_in_domain plain "$IDBIN" -u)
[ "$rc" = "0" ] && wait_audit 'ALLOW.*plain' \
    && pass "T7 audit 打开后 ALLOW 事件可见" || fail "T7 ALLOW 事件未出现"
"$CTL" --socket "$CTL_SOCK" set --audit-exec off >/dev/null 2>&1

"$CTL" --socket "$CTL_SOCK" set --enable-exec off >/dev/null 2>&1 \
    || fail "T8 set --enable-exec off 调用失败"
rc=$(run_in_domain web "$IDBIN" -u)
[ "$rc" = "0" ] && pass "T8 enable-exec=off 后 exec 维 fail-open" || fail "T8 关闭后仍被拦"
"$CTL" --socket "$CTL_SOCK" set --enable-exec on >/dev/null 2>&1
rc=$(run_in_domain web "$IDBIN")
[ "$rc" != "0" ] && pass "T8 enable-exec=on 恢复后拦截恢复" || fail "T8 恢复后未拦截"

echo
echo "== T9: 整份热更新 (reload --file) =="
cat > "$TDIR/policy-v2.yaml" <<YAML
rules:
  - file deny+audit ${VICTIM}
  - file deny+audit ${VICTIM2}
groups:
  ops:
    rules:
      - exec deny ${IDBIN}
domains:
  - name: web
    group: ops
  - name: plain
YAML
rc_before=$(run_in_domain plain cat "$VICTIM2")
"$CTL" --socket "$CTL_SOCK" reload --file "$TDIR/policy-v2.yaml" >/dev/null 2>&1 \
    || fail "T9 reload 调用失败"
rc_after=$(run_in_domain plain cat "$VICTIM2")
[ "$rc_before" = "0" ] && [ "$rc_after" != "0" ] \
    && pass "T9 reload 后新基线规则生效" \
    || fail "T9 热更新未生效 (before=$rc_before after=$rc_after)"

echo
echo "== T10: 组级热更新 (policy apply + bind 换绑) =="
printf 'rules:\n  - exec deny %s\n' "$WHOAMIBIN" | \
    "$CTL" --socket "$CTL_SOCK" policy apply --name ops2 --stdin >/dev/null 2>&1 \
    || fail "T10 policy apply ops2 调用失败"
"$CTL" --socket "$CTL_SOCK" bind web --group ops2 >/dev/null 2>&1 \
    || fail "T10 bind web -> ops2 失败"
rc=$(run_in_domain web "$WHOAMIBIN")
[ "$rc" != "0" ] && pass "T10 换绑后 ops2 规则生效 (whoami 被拦)" || fail "T10 whoami 未被拦"
rc=$(run_in_domain web "$IDBIN" -u)
[ "$rc" = "0" ] && pass "T10 整组替换: 旧组 ops 的 id 规则已失效" || fail "T10 整组替换语义不成立"
"$CTL" --socket "$CTL_SOCK" bind web --group ops >/dev/null 2>&1

echo
echo "== T11: 旧格式 YAML 被拒且旧策略保留 =="
printf 'enable_file: true\n' | "$CTL" --socket "$CTL_SOCK" reload --stdin >/dev/null 2>&1 \
    && fail "T11 旧格式竟被接受" || pass "T11 旧格式 (enable_file) 被 reload 拒绝"
rc=$(run_in_domain web cat "$VICTIM")
[ "$rc" != "0" ] && pass "T11 失败后旧策略保留" || fail "T11 失败后策略失效"

echo
echo "== T12: attach --pid 纳管已运行进程 =="
GO=$TDIR/go
bash -c 'while [ ! -f "$0" ]; do sleep 0.1; done
         cat "$1" >/dev/null 2>&1; echo "rc=$?" > "$2/rcA"' "$GO" "$VICTIM" "$OUT" &
BPID=$!
sleep 0.3
if "$CTL" --socket "$CTL_SOCK" attach --pid "$BPID" --domain web >/dev/null 2>&1; then
    touch "$GO"
    for _ in $(seq 1 50); do [ -s "$OUT/rcA" ] && break; sleep 0.1; done
    . "$OUT/rcA"
    [ "${rc:-0}" != "0" ] && pass "T12 attach 后新子孙 exec 被拦" || fail "T12 attach 树未被拦 (rc=$rc)"
else
    fail "T12 attach RPC 失败"
    kill "$BPID" 2>/dev/null
fi

echo
echo "== T13: 根退出后追踪集清理 =="
if [ "$HAVE_BPFTOOL" = 1 ]; then
    TRACK_OUT=$("$CTL" --socket "$CTL_SOCK" spawn --domain plain -- sleep 0.3 2>/dev/null)
    TRACK_PID=$(echo "$TRACK_OUT" | grep -o 'tracking pid=[0-9]*' | grep -o '[0-9]*' | head -1)
    sleep 1
    pid_key() { printf '%d %d %d %d' $(($1 & 255)) $(($1 >> 8 & 255)) $(($1 >> 16 & 255)) $(($1 >> 24 & 255)); }
    if [ -n "$TRACK_PID" ] && \
       bpftool map lookup name tracked_pids key $(pid_key "$TRACK_PID") >/dev/null 2>&1; then
        fail "T13 根退出后 tracked_pids 仍残留 pid=$TRACK_PID"
    else
        pass "T13 根退出后追踪集已清理"
    fi
else
    skip "T13 (无 bpftool)"
fi

echo
echo "== T14: daemon 未起时的 ctl 失败语义 =="
"$CTL" --socket "$TDIR/no-daemon.sock" status >/dev/null 2>&1 \
    && fail "T14 daemon 未起时 status 竟成功" || pass "T14 daemon 未起时 ctl 非 0 退出"

echo
echo "==================================================="
if [ "$FAILS" -eq 0 ]; then
    echo "全部通过。fixtures 保留在 $TDIR (可自行删除)"
    exit 0
else
    echo "$FAILS 项失败。daemon/audit 日志与 fixtures 在 $TDIR"
    exit 1
fi
