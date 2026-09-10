#!/usr/bin/env bash
# 构建浏览器半 client bundle (DSH ModuleLoader closure-factory 格式):
#   window.__ModuleLoader__.load({ id, factory: (require) => { ... } })
# react / cordis 等保持 external, 运行时由 DSH 平台模块表解析
# (deepseek-harness packages/client/web/src/platform.ts PLATFORM_MODULES)。
#
# esbuild 是本包的 devDependency。注意 pnpm 的 .bin shim 固定经 node 调用,
# 而 esbuild postinstall 会把 bin/esbuild 覆盖为平台 ELF 二进制 (shim 会炸),
# 所以绕过 shim 直接定位包内真实文件 (JS wrapper 有 shebang, ELF 可直接 exec,
# 两种形态都能跑)。可用 ESBUILD_BIN 环境变量覆盖。
set -euo pipefail
cd "$(dirname "$0")/.."

ESBUILD_BIN="${ESBUILD_BIN:-$(node -p 'require("node:path").join(require("node:path").dirname(require.resolve("esbuild/package.json")), "bin/esbuild")')}"
PKG_ID='@censorguard/dsh'

"$ESBUILD_BIN" src/client/index.ts \
  --bundle --format=cjs --target=es2022 --jsx=automatic \
  --external:react --external:react/jsx-runtime \
  --external:react-dom --external:react-dom/client \
  --external:@deepseek-ai/cordis \
  --external:@deepseek-ai/dsh-client-ui-slots \
  --banner:js="window.__ModuleLoader__.load({id:\"$PKG_ID\",factory:(require)=>{var module={exports:{}};var exports=module.exports;" \
  --footer:js="return module.exports}})" \
  --outfile=lib/client.js

echo "[+] built lib/client.js"
