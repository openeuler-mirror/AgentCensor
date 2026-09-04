#!/usr/bin/env bash
# Censorguard DSH bundle 安装/卸载验收 (免 root, 需要真实 DSH 环境)
# 对照 ohmyguard C4-C7, 但走真实机制: dsh plugin add/remove + --dump-config
# (本项目 install.mjs 是 DSH CLI 的薄封装, 不直接改 profile 文件)。
#
# 验证点:
#   D1  install.mjs 装入 4 个 @censorguard 包 + --dump-config 验证通过
#   D2  dump-config.mjs: 三插件行 + 8 个 censorguardReady 闸门全部 ✓
#   D3  幂等: 重复 install 报 "均已安装, 跳过 pnpm add"
#   D4  uninstall.mjs 移除后 dump 不再含 censorguard 行 (patch 层随包回落)
#   D5  现场恢复: 测试前已装则重装回, 未装则保持卸载态
#
# 用法: bash scripts/censorguard-dsh-bundle.sh [--profile <name>]
# 前置: ~/deepseek-harness-master 存在, ~/.dsh/profiles/<name> 已初始化

set -u
cd "$(dirname "$0")/.."
PROFILE=${2:-web}
DSH_REPO=$HOME/deepseek-harness-master
MANIFEST=$HOME/.dsh/profiles/$PROFILE/package.json
BUNDLE_BIN=plugins/dsh-censorguard/bundle/bin
FAILS=0
pass() { echo "  [PASS] $1"; }
fail() { echo "  [FAIL] $1"; FAILS=$((FAILS+1)); }

[ -f "$DSH_REPO/apps/cli/src/plugin.ts" ] || { echo "找不到 DSH 仓库: $DSH_REPO"; exit 1; }
[ -f "$MANIFEST" ] || { echo "profile 不存在: $MANIFEST"; exit 1; }

# 记录初始状态 (D5 恢复用)
WAS_INSTALLED=0
grep -q '@censorguard/dsh-bundle' "$MANIFEST" && WAS_INSTALLED=1
echo "  [INFO] profile=$PROFILE 初始已安装=$WAS_INSTALLED"

echo "=== D1: install.mjs 安装 + 自动 dump 验证 ==="
OUT=$(node "$BUNDLE_BIN/install.mjs" --profile "$PROFILE" 2>&1)
if [ $? -eq 0 ] && echo "$OUT" | grep -q '安装完成'; then
    pass "install.mjs 安装成功"
else
    fail "install.mjs 失败: $(echo "$OUT" | tail -5)"
fi

echo "=== D2: dump-config.mjs 逐项验证 ==="
OUT=$(node "$BUNDLE_BIN/dump-config.mjs" --profile "$PROFILE" 2>&1)
if [ $? -eq 0 ] && echo "$OUT" | grep -q '✓ Censorguard 已正确组装进 profile'; then
    pass "三插件行 + 8 个 inject 闸门全部 ✓"
else
    fail "dump-config 验证失败: $(echo "$OUT" | grep '✗' | head -5)"
fi

echo "=== D3: 幂等重装 ==="
OUT=$(node "$BUNDLE_BIN/install.mjs" --profile "$PROFILE" 2>&1)
if echo "$OUT" | grep -q '均已安装, 跳过 pnpm add'; then
    pass "重复 install 幂等跳过"
else
    fail "幂等检查异常: $(echo "$OUT" | tail -3)"
fi

echo "=== D4: uninstall.mjs 移除 + dump 回落 ==="
OUT=$(node "$BUNDLE_BIN/uninstall.mjs" --profile "$PROFILE" 2>&1)
if [ $? -eq 0 ] && echo "$OUT" | grep -q '已移除'; then
    DUMP_AFTER=$(cd "$DSH_REPO" && pnpm dsh --profile "$PROFILE" --dump-config 2>/dev/null)
    if ! echo "$DUMP_AFTER" | grep -q 'censorguard'; then
        pass "卸载后 dump 无 censorguard 残留 (patch 层随包回落)"
    else
        fail "卸载后 dump 仍含 censorguard 行"
    fi
else
    fail "uninstall.mjs 失败: $(echo "$OUT" | tail -5)"
fi

echo "=== D5: 卸载幂等 + 恢复初始状态 ==="
OUT=$(node "$BUNDLE_BIN/uninstall.mjs" --profile "$PROFILE" 2>&1)
echo "$OUT" | grep -q '无需移除 (幂等)' && pass "重复 uninstall 幂等" || fail "uninstall 不幂等: $OUT"
if [ "$WAS_INSTALLED" -eq 1 ]; then
    node "$BUNDLE_BIN/install.mjs" --profile "$PROFILE" >/dev/null 2>&1 \
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
