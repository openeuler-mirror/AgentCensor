import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { access, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout } from 'node:timers/promises'
import test from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { Session } from '@deepseek-ai/dsh-session'
import { WebDriverAgent } from '../lib/index.mjs'

const require = createRequire(join(process.env.DSH_ROOT, 'apps/cli/package.json'))
const yaml = require('js-yaml')

async function fixture(t, mode = 'ready', events = []) {
  const directory = await mkdtemp(join(tmpdir(), 'scope-worker-test-'))
  const binary = join(directory, 'dsh.mjs')
  await writeFile(binary, `#!${process.execPath}
import { createInterface } from 'node:readline'
import { writeFileSync } from 'node:fs'
writeFileSync(${JSON.stringify(join(directory, 'env.json'))}, JSON.stringify(process.env))
if (${JSON.stringify(mode)} === 'exit') { console.error('boot rejected'); process.exit(7) }
for await (const line of createInterface({input:process.stdin})) {
  const frame = JSON.parse(line)
  if (frame.type === 'seed' && frame.done && ${JSON.stringify(mode)} === 'ready') console.log('ACREADY\\t{}')
  if (frame.type === 'shutdown') process.exit(0)
}
`, { mode: 0o755 })
  const previous = { ...process.env }
  process.env.AGENTCENSOR_DSH_BIN = binary
  process.env.DSH_AGENTS_HOME = join(directory, 'user skills')
  const ctx = new Context()
  ctx.provide('sessions', { async flush() {} })
  ctx.provide('sessionPersistence', {
    async open() { return { async read() { return { events } }, async close() {} } },
  })
  const session = Session.create('session-a')
  const agent = new WebDriverAgent(ctx, session, { workerReadyTimeoutMs: mode === 'silent' ? 150 : 5000 })
  t.after(async () => {
    const state = agent.workerState
    if (state) { state.stop('SIGKILL'); await state.close }
    await agent.scope.dispose()
    for (const key of Object.keys(process.env)) if (!(key in previous)) delete process.env[key]
    Object.assign(process.env, previous)
    await rm(directory, { recursive: true, force: true })
  })
  return { agent, directory }
}

async function assertRemoved(path) {
  for (let attempt = 0; attempt < 100; attempt++) {
    try { await access(path) } catch (error) {
      assert.equal(error.code, 'ENOENT')
      return
    }
    await setTimeout(10)
  }
  assert.fail(`worker scratch directory still exists: ${path}`)
}

test('seed launch preserves user configuration and isolates writable session roots', async t => {
  const events = Array.from({ length: 1200 }, (_, seq) => ({ seq, data: 'x'.repeat(300) }))
  const { agent, directory } = await fixture(t, 'ready', events)
  const state = await agent.ensureWorker()
  assert.equal(state.mode, 'seed')
  const env = JSON.parse(await readFile(join(directory, 'env.json'), 'utf8'))
  assert.equal(env.DSH_AGENTS_HOME, process.env.DSH_AGENTS_HOME)
  assert.equal(env.CENSORSCOPE_SESSION_ID, agent.id)
  const rows = yaml.load(await readFile(join(state.privHome, 'overlay.yml'), 'utf8'))
  const byId = new Map(rows.map(row => [row.id, row]))
  for (const id of ['credentials', 'authorization', 'deepseek-account']) assert.equal(byId.get(id).disabled, false)
  assert.equal(byId.get('session-persistence-jsonl').config.root, join(state.privHome, 'sessions'))
  assert.equal(byId.get('headless-runner').disabled, true)
  assert.equal(await agent.ensureWorker(), state)
  agent.markBusy()
  agent.markBusy()
  const idle = agent.whenIdle()
  state.stop('shutdown')
  await state.close
  await idle
  await assertRemoved(state.privHome)
})

test('early exit rejects readiness and cleans the process and scratch directory', async t => {
  const { agent } = await fixture(t, 'exit')
  const ready = agent.ensureWorker()
  await assert.rejects(ready, /boot rejected/)
  assert.equal(agent.spawnFailures, 1)
  assert.equal(agent.workerState, undefined)
})

test('silent worker has bounded readiness and is terminated', async t => {
  const { agent } = await fixture(t, 'silent')
  const ready = agent.ensureWorker()
  while (!agent.workerState) await setTimeout(5)
  const state = agent.workerState
  await assert.rejects(ready, /readiness timeout/)
  await state.close
  await assertRemoved(state.privHome)
})

test('missing executable rejects instead of leaving an unhandled pipe error', async t => {
  const { agent, directory } = await fixture(t)
  process.env.AGENTCENSOR_DSH_BIN = join(directory, 'missing')
  await assert.rejects(agent.ensureWorker(), /ENOENT|EPIPE/)
})
