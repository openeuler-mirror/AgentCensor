import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { dshCommand } from '../scripts/ops/dsh.mjs';

test('DSH launch accepts official npm layouts, CLI symlinks and built source, including paths with spaces', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-dsh 路径 '));
  try {
    const prefix = join(directory, 'npm install');
    const pkg = join(prefix, 'node_modules/@deepseek-ai/dsh');
    const cli = join(pkg, 'lib/bin.js');
    const bin = join(prefix, 'node_modules/.bin/dsh');
    const source = join(directory, 'source');
    const sourceCli = join(source, 'apps/cli/lib/bin.js');
    for (const path of [join(pkg, 'lib'), join(prefix, 'node_modules/.bin'), join(source, 'apps/cli/lib')]) await mkdir(path, { recursive: true });
    await writeFile(cli, ''); await writeFile(sourceCli, ''); await symlink(cli, bin);
    for (const path of [prefix, pkg, cli, bin]) assert.deepEqual(await dshCommand(path), [process.execPath, cli]);
    assert.deepEqual(await dshCommand(source), [process.execPath, sourceCli]);
    assert.deepEqual(await dshCommand(''), ['dsh']);
    await assert.rejects(dshCommand(join(directory, 'unbuilt')), /dsh_path_invalid/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
