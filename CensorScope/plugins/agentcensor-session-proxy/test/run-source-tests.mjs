import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { mkdtemp, readdir, rm, writeFile } from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const root = resolve(process.env.DSH_ROOT ?? join(homedir(), 'deepseek-harness'))
const require = createRequire(join(root, 'package.json'))
const ts = require('typescript')
const { config, error } = ts.readConfigFile(join(root, 'tsconfig.base.json'), ts.sys.readFile)
if (error) throw new Error(ts.flattenDiagnosticMessageText(error.messageText, '\n'))
config.compilerOptions.paths = Object.fromEntries(
  Object.entries(config.compilerOptions.paths).map(([name, paths]) => [name, paths.map(path => resolve(root, path))]),
)
// A source checkout can have dependencies in pnpm's store before workspace links are refreshed.
const storeRequire = createRequire(join(root, 'node_modules/.pnpm/package.json'))
config.compilerOptions.paths.zod = [storeRequire.resolve('zod')]
const directory = await mkdtemp(join(tmpdir(), 'censorscope-dsh-tests-'))
try {
  const tsconfig = join(directory, 'tsconfig.json')
  await writeFile(tsconfig, JSON.stringify(config))
  const tests = (await readdir(import.meta.dirname)).filter(name => name.endsWith('.test.mjs'))
  const result = spawnSync(process.execPath, [
    '--import', require.resolve('tsx'), '--test', ...tests.map(name => join(import.meta.dirname, name)),
  ], {
    stdio: 'inherit', env: { ...process.env, DSH_ROOT: root, TSX_TSCONFIG_PATH: tsconfig },
  })
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
} finally {
  await rm(directory, { recursive: true, force: true })
}
