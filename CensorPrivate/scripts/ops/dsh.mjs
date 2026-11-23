import { stat, realpath } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function dshCommand(givenPath) {
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (!(major > 24 || major === 24 && minor >= 9)) throw new Error('dsh_requires_node_24.9_or_newer');
  if (!givenPath) return ['dsh'];
  const path = resolve(givenPath);
  // Official npm prefix/package/CLI and a built source checkout are supported.
  // Resolve npm's .bin symlink so the active (or bundled) Node runs the CLI.
  const candidates = [path, join(path, 'apps/cli/lib/bin.js'), join(path, 'lib/bin.js'),
    join(path, 'node_modules/@deepseek-ai/dsh/lib/bin.js')];
  for (const candidate of candidates) {
    const entry = await realpath(candidate).catch(() => undefined);
    if (entry && /\.(?:m?js)$/.test(entry) && (await stat(entry)).isFile()) return [process.execPath, entry];
  }
  throw new Error('dsh_path_invalid_use_npm_prefix_package_cli_or_built_source');
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  await dshCommand(process.argv[2]).catch(failure => { console.error(failure.message); process.exitCode = 1; });
}
