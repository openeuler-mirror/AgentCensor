#!/usr/bin/env bash
# Censorguard DSH 单包安装/卸载验收 (免 root, 需要真实 DSH 环境)
# 走 DSH 原生机制: dsh plugin add/remove + --dump-config
# (插件是单包 @censorguard/dsh, dsh.bundle 声明使其自动进入层列表)。
#
# 验证点:
#   D1  dsh plugin add 装入 @censorguard/dsh + --dump-config 验证通过
#   D2  dump-config.mjs: 三插件行 + 8 个 censorguardReady 闸门全部 ✓
#   D3  幂等: 重复 add 成功 (pnpm 对已装 link 依赖幂等)
#   D4  dsh plugin remove 后 dump 不再含 censorguard 行 (patch 层随包回落)
#   D5  现场恢复: 测试前已装则重装回, 未装则保持卸载态
#
# 用法: bash scripts/censorguard-dsh-bundle.sh [--profile <name>]
# 前置: ~/deepseek-harness-master 存在, ~/.dsh/profiles/<name> 已初始化

set -u
cd "$(dirname "$0")/.."
PROFILE=${2:-web}
DSH_REPO=$HOME/deepseek-harness-master
MANIFEST=$HOME/.dsh/profiles/$PROFILE/package.json
PLUGIN_DIR=$PWD/plugins/dsh-censorguard
DUMP_BIN=$PLUGIN_DIR/bin/dump-config.mjs
FAILS=0
pass() { echo "  [PASS] $1"; }
fail() { echo "  [FAIL] $1"; FAILS=$((FAILS+1)); }

[ -f "$DSH_REPO/apps/cli/src/plugin.ts" ] || { echo "找不到 DSH 仓库: $DSH_REPO"; exit 1; }
[ -f "$MANIFEST" ] || { echo "profile 不存在: $MANIFEST"; exit 1; }

dsh_plugin() { (cd "$DSH_REPO" && pnpm dsh plugin --profile "$PROFILE" "$@"); }

# 记录初始状态 (D5 恢复用)
WAS_INSTALLED=0
grep -q '"@censorguard/dsh"' "$MANIFEST" && WAS_INSTALLED=1
echo "  [INFO] profile=$PROFILE 初始已安装=$WAS_INSTALLED"

echo "=== D1: dsh plugin add 安装 ==="
OUT=$(dsh_plugin add "$PLUGIN_DIR" 2>&1)
if [ $? -eq 0 ] && grep -q '"@censorguard/dsh"' "$MANIFEST"; then
    pass "dsh plugin add 安装成功"
else
    fail "dsh plugin add 失败: $(echo "$OUT" | tail -5)"
fi

echo "=== D2: dump-config.mjs 逐项验证 ==="
OUT=$(node "$DUMP_BIN" --profile "$PROFILE" --dsh "$DSH_REPO" 2>&1)
if [ $? -eq 0 ] && echo "$OUT" | grep -q '✓ Censorguard 已正确组装进 profile'; then
    pass "三插件行 + 8 个 inject 闸门全部 ✓"
else
    fail "dump-config 验证失败: $(echo "$OUT" | grep '✗' | head -5)"
fi

echo "=== D3: 幂等重装 ==="
OUT=$(dsh_plugin add "$PLUGIN_DIR" 2>&1)
if [ $? -eq 0 ]; then
    pass "重复 add 幂等成功"
else
    fail "幂等检查异常: $(echo "$OUT" | tail -3)"
fi

echo "=== D4: dsh plugin remove + dump 回落 ==="
OUT=$(dsh_plugin remove @censorguard/dsh 2>&1)
if [ $? -eq 0 ]; then
    DUMP_AFTER=$(cd "$DSH_REPO" && pnpm dsh --profile "$PROFILE" --dump-config 2>/dev/null)
    if ! echo "$DUMP_AFTER" | grep -q 'censorguard'; then
        pass "卸载后 dump 无 censorguard 残留 (patch 层随包回落)"
    else
        fail "卸载后 dump 仍含 censorguard 行"
    fi
else
    fail "dsh plugin remove 失败: $(echo "$OUT" | tail -5)"
fi

echo "=== D5: 卸载幂等 + 恢复初始状态 ==="
# pnpm ≥10 对不存在的依赖 remove 报 ERR_PNPM_CANNOT_REMOVE_MISSING_DEPS:
# 效果上仍是幂等 (无东西可删), 接受该错误码为通过
OUT=$(dsh_plugin remove @censorguard/dsh 2>&1)
if [ $? -eq 0 ] || echo "$OUT" | grep -q 'ERR_PNPM_CANNOT_REMOVE_MISSING_DEPS'; then
    pass "重复 remove 幂等 (含 pnpm 无依赖可删情形)"
else
    fail "remove 不幂等: $OUT"
fi
if [ "$WAS_INSTALLED" -eq 1 ]; then
    dsh_plugin add "$PLUGIN_DIR" >/dev/null 2>&1 \
        && pass "已恢复为安装态 (测试前已装)" || fail "恢复安装失败"
else
    pass "保持卸载态 (测试前未装)"
fi

echo
if [ "$FAILS" -eq 0 ]; then
    echo "全部通过 (D1-D5)"
    exit 0
else
    echo "失败 $FAILS 项"
    exit 1
fi
