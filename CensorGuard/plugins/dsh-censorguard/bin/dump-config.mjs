#!/usr/bin/env node
// censorguard-dsh-dump-config: 验证 Censorguard 插件在 DSH Profile 的组装结果
// 真实机制 (deepseek-harness apps/cli/src/dump-config.ts): dsh --profile <name>
// --dump-config 离线组合 patch 层 (bundle 层 → profile 自己的 cordis.patch.yml
// → $DSH_HOME/cordis.patch.yml → --patch), 不启动服务、不求值 !!js。
// 本脚本薄封装它, 并 grep 验证 Censorguard 的三行插件 + inject 闸门都在。
// 纯验证工具: 安装/卸载用 DSH 原生命令
//   dsh plugin --profile web add /path/to/plugins/dsh-censorguard
//   dsh plugin --profile web remove @censorguard/dsh
//
// 用法: censorguard-dsh-dump-config [--profile <name>] [--dsh <repo>]
//   --profile  DSH profile 名 (默认 web)
//   --dsh      deepseek-harness 源码仓库路径
//
// dsh CLI 定位 (四级探测链, 取先命中者):
//   1. --dsh <repo>            源码仓库, 经 pnpm dsh 调用
//   2. $DSH_BIN                源码仓库目录或 dsh 可执行文件 (自动判别)
//   3. PATH 上的 dsh           全局安装
//   4. npx 联网调用            @deepseek-ai/dsh (会有下载提醒)
// 退出码: 0 = 验证通过; 1 = dump 失败或关键行缺失

import { parseArgs, resolveDsh, runDsh } from './lib/dsh.mjs';

// 必须出现在 dump 里的行: 三个插件行 + inject 闸门 (bundle patch 的
// cordis.patch.yml 声明, 见 bundle/cordis.patch.yml)
const REQUIRED_LINES = [
  'censorguard-bootstrap',
  'censorguard-host',
  'censorguard-ui',
];
const REQUIRED_INJECT_TARGETS = [
  'webserver',
  'web-runtime',
  'api-gateway',
  'cordis-host-runner',
  'code-runtime',
  'subprocess',
  'tools',
  'agent-loop',
];

function main() {
  const args = parseArgs(process.argv.slice(2));
  const profile = args.profile ?? 'web';
  const dsh = resolveDsh(args);

  console.log(`[dump-config] profile=${profile} dsh=${dsh.source} (${dsh.prefix.join(' ')})`);

  const dump = runDsh(dsh, ['--profile', profile, '--dump-config'], { capture: true });
  if (dump.status !== 0) {
    console.error(`[dump-config] ✗ dsh --dump-config 失败 (exit=${dump.status})`);
    process.exit(1);
  }
  const output = dump.output;

  console.log('=== Censorguard 验证 ===');
  let ok = true;

  for (const line of REQUIRED_LINES) {
    const found = output.includes(line);
    console.log(`  ${found ? '✓' : '✗'} 插件行 ${line}`);
    if (!found) ok = false;
  }

  for (const target of REQUIRED_INJECT_TARGETS) {
    // dump 是 yaml.dump 输出, inject 折行成列表 (不可用 [\] 紧凑形式):
    //   inject:
    //     - webStartup
    //     - censorguardReady
    // 匹配: target 的 id 行后 200 字符内出现 inject: 且其后跟 censorguardReady。
    const found = new RegExp(`- id: ${target}[\\s\\S]{0,200}?inject:[\\s\\S]{0,100}?censorguardReady`, 'm').test(output);
    console.log(`  ${found ? '✓' : '✗'} inject 闸门 ${target}`);
    if (!found) ok = false;
  }

  console.log(`整体: ${ok ? '✓ Censorguard 已正确组装进 profile' : '✗ 有缺失, 检查 bundle 是否安装/层序'}`);
  process.exit(ok ? 0 : 1);
}

main();
