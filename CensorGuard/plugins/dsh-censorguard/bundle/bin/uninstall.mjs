#!/usr/bin/env node
// censorguard-dsh-uninstall: 从 DSH Profile 移除 Censorguard 插件 bundle
// 真实机制 (deepseek-harness apps/cli/src/plugin.ts): dsh plugin remove 转发
// pnpm remove, 并按安装状态 reconcile: 从 dependencies 移除后,
// dsh.profile.bundles 层列表里的 @censorguard/dsh-bundle 也随之摘除。
// cordis.patch.yml 层消失 → inject 闸门/插件行全部回落到原始配置。
//
// 用法: censorguard-dsh-uninstall [--profile <name>] [--dsh <repo>] [--restore-backup]
//   --profile        DSH profile 名 (默认 web)
//   --dsh            deepseek-harness 仓库路径 (默认 ~/deepseek-harness-master)
//   --restore-backup 从 install 时的备份恢复 profile manifest (不做 pnpm remove)

import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { homedir } from 'node:os';

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

  const profileDir = join(homedir(), '.dsh/profiles', profile);
  const manifestPath = join(profileDir, 'package.json');

  console.log(`[uninstall] profile=${profile} dsh=${dshRepo}`);

  if (args['restore-backup'] === true) {
    const backupPath = manifestPath + '.censorguard-bak';
    if (!existsSync(backupPath)) {
      console.error(`[uninstall] ✗ 备份不存在: ${backupPath}`);
      process.exit(1);
    }
    copyFileSync(backupPath, manifestPath);
    console.log(`[uninstall] ✓ 已从备份恢复 ${manifestPath} (记得在 profile 目录 pnpm install 同步 node_modules)`);
    process.exit(0);
  }

  if (!existsSync(join(dshRepo, 'apps/cli/src/plugin.ts'))) {
    console.error(`[uninstall] ✗ 找不到 DSH CLI: ${dshRepo}`);
    process.exit(1);
  }
  if (!existsSync(manifestPath)) {
    console.error(`[uninstall] ✗ profile 不存在: ${manifestPath}`);
    process.exit(1);
  }

  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  const installed = PLUGIN_PKGS.filter((pkg) => manifest.dependencies?.[pkg] !== undefined);
  if (installed.length === 0) {
    console.log('[uninstall] @censorguard 包均未安装, 无需移除 (幂等)');
    process.exit(0);
  }

  // pnpm remove 支持一次传多个; bundle 层随之摘除, inject 闸门随层回落
  const remove = run('pnpm', ['dsh', 'plugin', '--profile', profile, 'remove', ...installed], { cwd: dshRepo });
  if (remove.status !== 0) {
    console.error('[uninstall] ✗ dsh plugin remove 失败 (见上方 pnpm 输出)');
    process.exit(remove.status ?? 1);
  }
  console.log('[uninstall] ✓ 已移除 (bundle 层摘除, inject 闸门随层消失)');
}

main();
