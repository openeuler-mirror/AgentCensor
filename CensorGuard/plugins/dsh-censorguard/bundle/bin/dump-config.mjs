#!/usr/bin/env node
// censorguard-dsh-dump-config: 验证 Censorguard 插件在 DSH Profile 的组装结果
// 真实机制 (deepseek-harness apps/cli/src/dump-config.ts): dsh --profile <name>
// --dump-config 离线组合 patch 层 (bundle 层 → profile 自己的 cordis.patch.yml
// → ~/.dsh/cordis.patch.yml → --patch), 不启动服务、不求值 !!js。
// 本脚本薄封装它, 并 grep 验证 Censorguard 的三行插件 + inject 闸门都在。
//
// 用法: censorguard-dsh-dump-config [--profile <name>] [--dsh <repo>]
//   --profile  DSH profile 名 (默认 web)
//   --dsh      deepseek-harness 仓库路径 (默认 ~/deepseek-harness-master)
// 退出码: 0 = 验证通过; 1 = dump 失败或关键行缺失

import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { homedir } from 'node:os';

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

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith('--')) {
      const key = a.slice(2);
      if (i + 1 < argv.length && !argv[i + 1].startsWith('--')) {
        out[key] = argv[++i];
      } else {
        out[key] = true;
      }
    }
  }
  return out;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const profile = args.profile ?? 'web';
  const dshRepo = resolve(args.dsh ?? join(homedir(), 'deepseek-harness-master'));

  console.log(`[dump-config] profile=${profile} dsh=${dshRepo}`);

  if (!existsSync(join(dshRepo, 'apps/cli/src/dump-config.ts'))) {
    console.error(`[dump-config] ✗ 找不到 DSH CLI: ${dshRepo}`);
    process.exit(1);
  }

  const dump = spawnSync('pnpm', ['dsh', '--profile', profile, '--dump-config'], {
    cwd: dshRepo,
    encoding: 'utf8',
  });
  if (dump.status !== 0) {
    console.error(`[dump-config] ✗ dsh --dump-config 失败 (exit=${dump.status})`);
    console.error(dump.stderr ?? '');
    process.exit(1);
  }
  const output = dump.stdout ?? '';
  console.log(output);

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
