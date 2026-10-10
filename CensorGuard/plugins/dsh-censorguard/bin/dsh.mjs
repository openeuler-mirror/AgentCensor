import { accessSync, constants, existsSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { delimiter, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

export function parseArgs(values) {
  const options = {};
  for (let i = 0; i < values.length; i++) {
    const key = values[i];
    if (!['--profile', '--dsh'].includes(key) || !values[i + 1] || values[i + 1].startsWith('--')) {
      throw new Error('usage: dump-config.mjs [--profile NAME] [--dsh REPO_OR_EXECUTABLE]');
    }
    if (options[key.slice(2)] !== undefined) throw new Error(`duplicate option ${key}`);
    options[key.slice(2)] = values[++i];
  }
  return options;
}

function executableOnPath(name) {
  for (const directory of (process.env.PATH ?? '').split(delimiter)) {
    const candidate = join(directory, name);
    try { accessSync(candidate, constants.X_OK); return candidate; } catch { /* Try the next PATH directory. */ }
  }
}

export function resolveDsh(options) {
  const supplied = options.dsh ?? process.env.DSH_BIN ?? executableOnPath('dsh');
  if (!supplied) throw new Error('DSH not found; pass --dsh /path/to/deepseek-harness or set DSH_BIN');
  const location = resolve(supplied);
  if (existsSync(location) && statSync(location).isDirectory()) {
    const entry = join(location, 'apps/cli/src/bin.ts');
    if (!existsSync(entry)) throw new Error(`DSH source entry not found: ${entry}`);
    const require = createRequire(join(location, 'package.json'));
    return { source: location, cwd: location, prefix: [process.execPath, '--import', require.resolve('tsx/esm'), entry] };
  }
  const command = existsSync(location) ? location : executableOnPath(supplied);
  if (!command) throw new Error(`DSH executable not found: ${supplied}`);
  return { source: command, prefix: [command] };
}

export function runDsh(dsh, arguments_, options = {}) {
  const result = spawnSync(dsh.prefix[0], [...dsh.prefix.slice(1), ...arguments_], {
    cwd: dsh.cwd, encoding: 'utf8', timeout: 60000,
    stdio: options.capture ? 'pipe' : 'inherit',
  });
  if (result.error) throw result.error;
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}
