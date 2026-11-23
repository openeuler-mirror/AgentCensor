#!/usr/bin/env node
// censorguard-dsh-dump-config: 验证 Censorguard 插件在 DSH Profile 的组装结果
// 真实机制 (deepseek-harness apps/cli/src/dump-config.ts): dsh --profile <name>
// --dump-config 离线组合 patch 层 (bundle 层 → profile 自己的 cordis.patch.yml
// → $DSH_HOME/cordis.patch.yml → --patch), 不启动服务、不求值 !!js。
// 本脚本薄封装它, 并校验 Censorguard 的三行插件 + inject 闸门都在。
// 纯验证工具: 安装/卸载用 DSH 原生命令
//   dsh plugin --profile web add /path/to/plugins/dsh-censorguard
//   dsh plugin --profile web remove @censorguard/dsh
//
// 用法: censorguard-dsh-dump-config [--profile <name>] [--dsh <repo>] [--check-targets]
//   --profile        DSH profile 名 (默认 web)
//   --dsh            deepseek-harness 源码仓库路径
//   --check-targets  只校验闸门目标在该 dsh 版本的真实 entry 集中存在
//                    (版本契约校验, 不要求本插件已安装)
//
// dsh CLI 定位 (三级探测链, 取先命中者):
//   1. --dsh <repo>            源码仓库, 经 tsx 直起 apps/cli/src/bin.ts
//   2. $DSH_BIN                源码仓库目录或 dsh 可执行文件 (自动判别)
//   3. PATH 上的 dsh           全局安装
// 缺少 DSH 时提示指定源码仓库, 不隐式下载另一个版本。
//
// 退出码 (默认模式): 0 = 验证通过; 1 = dump 失败或关键行缺失
// 退出码 (--check-targets): 0 = 闸门目标都在; 1 = 有目标不存在 (多半是 entry
//   改名); 2 = 取不到 dump (dsh 不可用 / profile 未初始化), 供离线套件跳过

import { parseArgs, resolveDsh, runDsh } from './dsh.mjs';

// 必须出现在 dump 里的行: 三个插件行 + inject 闸门 (bundle patch 的
// cordis.patch.yml 声明, 见 bundle/cordis.patch.yml)
const REQUIRED_LINES = [
  'censorguard-bootstrap',
  'censorguard-host',
  'censorguard-ui',
];
// 闸门目标必须同时存在于本 dsh 版本的真实 entry 集。静态 grep 查不出改名:
// applyEntryPatches 对不存在的 id 只 warn (patch: entry %C not found) 后
// continue, 闸门会静默空转。ptc-runtime 就是从 code-runtime 改名而来。
const REQUIRED_INJECT_TARGETS = [
  'webserver',
  'web-runtime',
  'typert-gateway',
  'cordis-host-runner',
  'ptc-runtime',
  'subprocess',
  'tools',
  'agent-loop',
];

// dump 是 yaml.dump 的顶层 entry 数组, 每个 entry 首行形如 "- id: <name>"
// (嵌套列表带缩进, 行首不是 "- ", 因此不会被误收)。
function collectEntryIds(output) {
  const ids = new Set();
  for (const match of output.matchAll(/^- id: (\S+)$/gm)) {
    ids.add(match[1].replace(/^["']|["']$/g, ''));
  }
  return ids;
}

// 取某个顶层 entry 的文本块: 从 "- id: <target>" 到下一个顶层 "- id: " 之前。
// 不能用"id 行后 N 个字符内出现 inject"这种窗口匹配 —— 目标自身没有 inject
// 时, 窗口会串到下一个 entry 的 inject 上, 把缺闸门误判成通过。
function entryBlock(output, target) {
  const start = output.search(new RegExp(`^- id: ${target}$`, 'm'));
  if (start < 0) return undefined;
  const rest = output.slice(start + 1);
  const next = rest.search(/^- id: /m);
  return next < 0 ? rest : rest.slice(0, next);
}

// inject 折行成列表:
//   inject:
//     - webStartup
//     - censorguardReady
// 也兼容单行 [webStartup, censorguardReady] 形式。
function hasGate(output, target) {
  const block = entryBlock(output, target);
  return block !== undefined && /inject:[\s\S]*?censorguardReady\b/.test(block);
}

function tail(text, lines = 3) {
  return text.trimEnd().split('\n').slice(-lines).join(' | ');
}

function main() {
  const raw = process.argv.slice(2);
  const checkTargets = raw.includes('--check-targets');
  const args = parseArgs(raw.filter(value => value !== '--check-targets'));
  const profile = args.profile ?? 'web';

  let dsh;
  try {
    dsh = resolveDsh(args);
  } catch (error) {
    if (checkTargets) {
      console.error(`[dump-config] SKIP: ${error.message}`);
      process.exit(2);
    }
    throw error;
  }

  console.log(`[dump-config] profile=${profile} dsh=${dsh.source} (${dsh.prefix.join(' ')})`);

  const dump = runDsh(dsh, ['--profile', profile, '--dump-config'], { capture: true });
  if (dump.status !== 0) {
    if (checkTargets) {
      // 取不到 dump 是环境问题 (无 profile / 依赖未装), 不等于契约不符
      console.error(`[dump-config] SKIP: dsh --dump-config 失败 (exit=${dump.status}), 取不到 entry 集`);
      console.error(`  ${tail(dump.output)}`);
      process.exit(2);
    }
    console.error(`[dump-config] ✗ dsh --dump-config 失败 (exit=${dump.status})`);
    process.exit(1);
  }
  const output = dump.output;
  const ids = collectEntryIds(output);

  if (checkTargets) {
    console.log(`=== 闸门目标存在性 (dump 顶层 entry ${ids.size} 个) ===`);
    const missing = [];
    for (const target of REQUIRED_INJECT_TARGETS) {
      const found = ids.has(target);
      console.log(`  ${found ? '✓' : '✗'} ${target}`);
      if (!found) missing.push(target);
    }
    if (missing.length > 0) {
      console.error(`  该 dsh 版本不存在: ${missing.join(', ')}`);
      console.error('  entry 多半已改名, 需同步 cordis.patch.yml 与 REQUIRED_INJECT_TARGETS');
      process.exit(1);
    }
    console.log('整体: ✓ 闸门目标在真实 dump 中全部存在');
    process.exit(0);
  }

  console.log('=== Censorguard 验证 ===');
  let ok = true;

  // patch 落空的 id 是"静默空转"的直接证据: applyEntryPatches 只 warn 不中断
  const patchMisses = [...output.matchAll(/patch: entry (\S+) not found/g)]
    .map(match => match[1].replace(/^["']|["']$/g, ''));
  if (patchMisses.length > 0) {
    console.error(`  ✗ patch 目标不存在: ${patchMisses.join(', ')} (检查 DSH 与插件版本)`);
    ok = false;
  }

  for (const line of REQUIRED_LINES) {
    const found = output.includes(line);
    console.log(`  ${found ? '✓' : '✗'} 插件行 ${line}`);
    if (!found) ok = false;
  }

  for (const target of REQUIRED_INJECT_TARGETS) {
    if (!ids.has(target)) {
      console.error(`  ✗ inject 闸门 ${target} —— 该 dsh 版本无此 entry`);
      ok = false;
      continue;
    }
    const found = hasGate(output, target);
    console.log(`  ${found ? '✓' : '✗'} inject 闸门 ${target}`);
    if (!found) ok = false;
  }

  console.log(`整体: ${ok ? '✓ Censorguard 已正确组装进 profile' : '✗ 有缺失, 检查 bundle 是否安装/层序'}`);
  process.exit(ok ? 0 : 1);
}

try { main(); } catch (error) {
  console.error(`[dump-config] ${error.message}`);
  process.exitCode = 1;
}
