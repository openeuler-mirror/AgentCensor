#!/usr/bin/env bash
# 构建浏览器半 client bundle (DSH ModuleLoader closure-factory 格式):
#   window.__ModuleLoader__.load({ id, factory: (require) => { ... } })
# react / cordis 等保持 external, 运行时由 DSH 平台模块表解析
# (deepseek-harness packages/client/web/src/platform.ts PLATFORM_MODULES)。
#
# esbuild 不在本 workspace 的依赖里, 默认借用 DSH 仓库的:
#   ESBUILD_BIN=${ESBUILD_BIN:-~/deepseek-harness-master/node_modules/.bin/esbuild}
set -euo pipefail
cd "$(dirname "$0")/.."

ESBUILD_BIN="${ESBUILD_BIN:-$HOME/deepseek-harness-master/node_modules/.bin/esbuild}"
PKG_ID='@censorguard/dsh-client'

"$ESBUILD_BIN" src/client/index.ts \
  --bundle --format=cjs --target=es2022 --jsx=automatic \
  --external:react --external:react/jsx-runtime \
  --external:react-dom --external:react-dom/client \
  --external:@deepseek-ai/cordis \
  --external:@deepseek-ai/dsh-client-ui-slots \
  --banner:js="window.__ModuleLoader__.load({id:\"$PKG_ID\",factory:(require)=>{var module={exports:{}};var exports=module.exports;" \
  --footer:js="return module.exports}})" \
  --outfile=lib/client.js

echo "[+] built packages/client/lib/client.js"
