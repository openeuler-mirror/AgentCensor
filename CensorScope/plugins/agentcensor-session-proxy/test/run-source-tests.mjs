import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { existsSync } from 'node:fs'
import { cp, mkdtemp, readdir, rm, symlink, writeFile } from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const root = resolve(process.env.DSH_ROOT ?? join(homedir(), 'deepseek-harness'))
const require = createRequire(join(root, 'package.json'))
const directory = await mkdtemp(join(tmpdir(), 'censorscope-dsh-tests-'))
try {
  const nodeArgs = []
  const env = { ...process.env, DSH_ROOT: root }
  let testDirectory = import.meta.dirname
  if (existsSync(join(root, 'tsconfig.base.json'))) {
    const ts = require('typescript')
    const { config, error } = ts.readConfigFile(join(root, 'tsconfig.base.json'), ts.sys.readFile)
    if (error) throw new Error(ts.flattenDiagnosticMessageText(error.messageText, '\n'))
    config.compilerOptions.paths = Object.fromEntries(
      Object.entries(config.compilerOptions.paths).map(([name, paths]) => [name, paths.map(path => resolve(root, path))]),
    )
    // Source checkouts may have dependencies in the store before workspace links are refreshed.
    const storeRequire = createRequire(join(root, 'node_modules/.pnpm/package.json'))
    config.compilerOptions.paths.zod = [storeRequire.resolve('zod')]
    env.TSX_TSCONFIG_PATH = join(directory, 'tsconfig.json')
    await writeFile(env.TSX_TSCONFIG_PATH, JSON.stringify(config))
    nodeArgs.push('--import', require.resolve('tsx'))
  } else {
    // Resolve the tests against the installed release without changing its files.
    testDirectory = join(directory, 'test')
    await cp(import.meta.dirname, testDirectory, { recursive: true })
    await cp(join(import.meta.dirname, '../lib'), join(directory, 'lib'), { recursive: true })
    await symlink(join(root, 'node_modules'), join(directory, 'node_modules'), 'junction')
  }
  const tests = (await readdir(testDirectory)).filter(name => name.endsWith('.test.mjs'))
  const result = spawnSync(process.execPath, [
    ...nodeArgs, '--test', ...tests.map(name => join(testDirectory, name)),
  ], {
    stdio: 'inherit', env,
  })
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
} finally {
  await rm(directory, { recursive: true, force: true })
}
