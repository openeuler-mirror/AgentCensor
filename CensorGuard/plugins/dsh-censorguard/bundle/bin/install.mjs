#!/usr/bin/env node
// censorguard-dsh-install: 把 Censorguard 插件 bundle 安装进 DSH Profile
// 真实机制 (deepseek-harness apps/cli/src/plugin.ts): dsh plugin add 本质是
// profile 目录里跑 pnpm add, 装了带 dsh.bundle 声明的包自动加入
// dsh.profile.bundles 层列表 (按安装状态 reconcile)。
// 本脚本是薄封装: 备份 profile package.json → dsh plugin add → dsh --dump-config 验证。
//
// 用法: censorguard-dsh-install [--profile <name>] [--dsh <repo>] [--dry-run]
//   --profile  DSH profile 名 (默认 web)
//   --dsh      deepseek-harness 仓库路径 (默认 ~/deepseek-harness-master)
//   --dry-run  只打印将执行的命令, 不改动

import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { homedir } from 'node:os';

const BUNDLE_PATH = resolve(new URL('.', import.meta.url).pathname, '..');
// loader entry name 与 client-modules 的包解析都从 profile 根起,
// 三个插件包必须与 bundle 一样是 profile 直接依赖 (只装 bundle 时
// 其 file: 传递依赖埋在 bundle 子 node_modules, profile 根解析不到)。
// runtime 包不是 entry, 作为 bootstrap/host 的传递依赖随装即可。
const PLUGIN_PATHS = [
  BUNDLE_PATH,
  resolve(BUNDLE_PATH, '../packages/bootstrap'),
  resolve(BUNDLE_PATH, '../packages/host'),
  resolve(BUNDLE_PATH, '../packages/client'),
];
const BUNDLE_PKG = '@censorguard/dsh-bundle';
const PLUGIN_PKGS = [
  BUNDLE_PKG,
  '@censorguard/dsh-bootstrap',
  '@censorguard/dsh-host',
  '@censorguard/dsh-client',
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

function run(cmd, args, opts = {}) {
  console.log(`$ ${cmd} ${args.join(' ')}`);
  return spawnSync(cmd, args, { stdio: 'inherit', ...opts });
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const profile = args.profile ?? 'web';
  const dshRepo = resolve(args.dsh ?? join(homedir(), 'deepseek-harness-master'));
  const dryRun = args.dryRun === true;

  const profileDir = join(homedir(), '.dsh/profiles', profile);
  const manifestPath = join(profileDir, 'package.json');

  console.log(`[install] profile=${profile} dsh=${dshRepo} bundle=${BUNDLE_PATH}`);
  console.log(`[install] dryRun=${dryRun}`);

  if (!existsSync(join(dshRepo, 'apps/cli/src/plugin.ts'))) {
    console.error(`[install] ✗ 找不到 DSH CLI: ${dshRepo}`);
    process.exit(1);
  }
  if (!existsSync(manifestPath)) {
    console.error(`[install] ✗ profile 不存在: ${manifestPath}`);
    console.error('  先用 dsh 初始化 profile, 或指定正确名字: --profile <name>');
    process.exit(1);
  }

  // 幂等: 全部四个包已在 dependencies 里则跳过安装
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  const missing = PLUGIN_PKGS.filter((pkg) => manifest.dependencies?.[pkg] === undefined);
  if (missing.length === 0) {
    console.log('[install] 四个 @censorguard 包均已安装, 跳过 pnpm add');
  } else if (dryRun) {
    console.log(`[install] dry-run: 将执行 pnpm dsh plugin --profile ${profile} add ${PLUGIN_PATHS.join(' ')}`);
    console.log(`[install] dry-run: 将执行 pnpm dsh --profile ${profile} --dump-config`);
  } else {
    // 备份 (手册 §6.5: Profile 备份和恢复)
    const backupPath = manifestPath + '.censorguard-bak';
    copyFileSync(manifestPath, backupPath);
    console.log(`[install] 备份 profile manifest → ${backupPath}`);

    // dsh plugin --profile <name> add <路径...>: pnpm add + reconcile bundles 层列表
    const add = run('pnpm', ['dsh', 'plugin', '--profile', profile, 'add', ...PLUGIN_PATHS], { cwd: dshRepo });
    if (add.status !== 0) {
      console.error('[install] ✗ dsh plugin add 失败 (见上方 pnpm 输出)');
      console.error(`  可从备份恢复: cp ${backupPath} ${manifestPath}`);
      process.exit(add.status ?? 1);
    }
  }

  // 验证: --dump-config 离线组合 patch 层, 不启动服务
  if (!dryRun) {
    const dump = run('pnpm', ['dsh', '--profile', profile, '--dump-config'], { cwd: dshRepo });
    if (dump.status !== 0) {
      console.error('[install] ✗ --dump-config 失败');
      process.exit(dump.status ?? 1);
    }
    console.log('[install] ✓ 安装完成 (上方 dump 输出应含 censorguard-bootstrap/host/ui 三行)');
  }
}

main();
