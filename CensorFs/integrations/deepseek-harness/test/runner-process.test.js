import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, symlink, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { NamespaceRunner, sandboxEnvironment } from '../src/runner-process.js'

async function createRunner() {
  const workspace = await mkdtemp(join(tmpdir(), 'censorfs-runner-'))
  const runner = new NamespaceRunner({ runnerId: 'runner-test', viewId: 'view-test', workspace })
  await runner.initialize()
  return { runner, workspace }
}

test('sandbox environment forwards only the explicit safe allowlist', () => {
  const environment = sandboxEnvironment({ PATH: '/safe/bin', HOME: '/safe/home', DEEPSEEK_API_KEY: 'secret', AWS_SECRET_ACCESS_KEY: 'secret' })
  assert.equal(environment.PATH, '/safe/bin')
  assert.equal(environment.HOME, '/safe/home')
  assert.equal(environment.DEEPSEEK_API_KEY, undefined)
  assert.equal(environment.AWS_SECRET_ACCESS_KEY, undefined)
})
test('runner reads, writes, and atomically edits workspace files', async () => {
  const { runner, workspace } = await createRunner()
  const created = await runner.dispatch('fs.write', {
    file_path: '/workspace/src/example.txt',
    content: 'one\ntwo\n',
  }, 'write-1')
  assert.equal(created.operation, 'create')
  assert.equal(await readFile(join(workspace, 'src', 'example.txt'), 'utf8'), 'one\ntwo\n')

  const read = await runner.dispatch('fs.read', { file_path: 'src/example.txt', offset: 2, limit: 1 }, 'read-1')
  assert.deepEqual(read.lines, [{ number: 2, text: 'two' }])

  const edited = await runner.dispatch('fs.edit', {
    file_path: 'src/example.txt',
    old_string: 'two',
    new_string: 'three',
  }, 'edit-1')
  assert.equal(edited.after, 'one\nthree\n')
})

test('runner rejects parent traversal, host absolute paths, and escaping symlinks', async () => {
  const { runner, workspace } = await createRunner()
  await assert.rejects(() => runner.resolvePath('../escape', { allowMissing: true }), { code: 'PATH_OUTSIDE_WORKSPACE' })
  await assert.rejects(() => runner.resolvePath('/etc/passwd'), { code: 'PATH_OUTSIDE_WORKSPACE' })
  if (process.platform !== 'win32') {
    await mkdir(join(workspace, 'links'))
    await symlink('/etc', join(workspace, 'links', 'outside'))
    await assert.rejects(() => runner.resolvePath('links/outside/passwd'), { code: 'PATH_OUTSIDE_WORKSPACE' })
    await symlink('/etc/passwd', join(workspace, 'links', 'leaf'))
    await assert.rejects(() => runner.dispatch('fs.write', {
      file_path: 'links/leaf', content: 'forbidden',
    }, 'write-symlink'), { code: 'PATH_OUTSIDE_WORKSPACE' })
  }
})

test('runner enforces unique edit by default and supports replace_all', async () => {
  const { runner, workspace } = await createRunner()
  await writeFile(join(workspace, 'repeated.txt'), 'x x')
  await assert.rejects(() => runner.dispatch('fs.edit', {
    file_path: 'repeated.txt', old_string: 'x', new_string: 'y',
  }, 'edit-ambiguous'), { code: 'EDIT_NOT_UNIQUE' })
  const result = await runner.dispatch('fs.edit', {
    file_path: 'repeated.txt', old_string: 'x', new_string: 'y', replace_all: true,
  }, 'edit-all')
  assert.equal(result.after, 'y y')
})

test('runner executes foreground bash in the requested workspace directory', { skip: process.platform === 'win32' }, async () => {
  const { runner, workspace } = await createRunner()
  await mkdir(join(workspace, 'sub'))
  const result = await runner.dispatch('process.run', {
    command: 'printf "%s" "$PWD"; printf "warn" >&2',
    workdir: '/workspace/sub',
    timeoutMs: 5000,
  }, 'bash-1')
  assert.equal(result.exitCode, 0)
  assert.equal(result.stdout.text, '/workspace/sub')
  assert.equal(result.stderr.text, 'warn')
})

test('runner executes and controls background jobs', { skip: process.platform === 'win32' }, async () => {
  const { runner } = await createRunner()
  const started = await runner.dispatch('process.run', {
    command: 'printf first; sleep 0.2; printf second', run_in_background: true,
  }, 'bash-bg')
  assert.equal(started.kind, 'background')
  assert.equal((await runner.dispatch('job.list', {}, 'jobs-list')).length, 1)
  const finished = await runner.dispatch('job.output', {
    job_id: started.jobId, wait: true, timeout_ms: 5000,
  }, 'jobs-output')
  assert.equal(finished.text, 'firstsecond')
  assert.equal(finished.job.status, 'completed')
  const consumed = await runner.dispatch('job.output', { job_id: started.jobId }, 'jobs-output-2')
  assert.equal(consumed.text, '')

  const long = await runner.dispatch('process.run', {
    command: 'sleep 30', run_in_background: true,
  }, 'bash-bg-long')
  const killed = await runner.dispatch('job.kill', { job_id: long.jobId }, 'jobs-kill')
  assert.equal(killed.outcome, 'cancellation-requested')
  const settled = await runner.dispatch('job.output', {
    job_id: long.jobId, wait: true, timeout_ms: 5000,
  }, 'jobs-killed-output')
  assert.equal(settled.job.status, 'killed')
})

test('runner refuses delegated sandbox escalation', async () => {
  const { runner } = await createRunner()
  await assert.rejects(() => runner.dispatch('process.run', {
    command: 'true', sandbox_permissions: 'danger-full-access', justification: 'test',
  }, 'bash-approval'), { code: 'APPROVAL_UNAVAILABLE' })
})
test('runner returns workspace-relative nested glob paths and treats grep patterns as data', async () => {
  const { runner, workspace } = await createRunner()
  await mkdir(join(workspace, 'nested'))
  await writeFile(join(workspace, 'nested', 'dash.txt'), '--pre=forbidden\n')
  const glob = await runner.dispatch('fs.glob', { pattern: '*.txt', path: '/workspace/nested' }, 'glob-nested')
  assert.deepEqual(glob.paths, ['nested/dash.txt'])
  const grep = await runner.dispatch('fs.grep', { pattern: '--pre=forbidden', path: '/workspace/nested' }, 'grep-dash')
  assert.equal(grep.matches.length, 1)
  assert.equal(grep.matches[0].path, 'nested/dash.txt')
})

test('runner validates read offsets and process timeouts', async () => {
  const { runner, workspace } = await createRunner()
  await writeFile(join(workspace, 'one-line.txt'), 'one\n')
  await assert.rejects(() => runner.dispatch('fs.read', {
    file_path: 'one-line.txt', offset: 2,
  }, 'read-offset'), { code: 'NOT_FOUND' })
  await assert.rejects(() => runner.dispatch('process.run', {
    command: 'true', timeoutMs: 0,
  }, 'bash-timeout'), { code: 'INVALID_REQUEST' })
})
