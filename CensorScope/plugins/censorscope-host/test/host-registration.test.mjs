import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtemp, readFile, readlink, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout } from 'node:timers/promises'
import test from 'node:test'

if (process.argv.includes('--host')) {
  const { apply } = await import('../lib/index.mjs')
  apply({
    get(name) {
      if (name === 'shellEnv' || name === 'webServer') return { register() { return () => {} } }
    },
    on() {},
    effect() {},
  })
  const record = join(process.env.DSH_HOME, '.censorscope', 'trace.json')
  for (let attempt = 0; ; attempt++) {
    try {
      const trace = JSON.parse(await readFile(record, 'utf8'))
      assert.equal(trace.rootPid, process.pid)
      assert.equal(trace.traceId, '17')
      break
    } catch (error) {
      if (attempt >= 100) throw error
      await setTimeout(20)
    }
  }
} else {
  test('host sends its own PID namespace before persisting the registered trace', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'scope-host-registration-'))
    try {
      const ctl = join(directory, 'ctl.mjs')
      const calls = join(directory, 'calls.jsonl')
      await writeFile(ctl, `#!${process.execPath}
import { appendFileSync } from 'node:fs'
const args = process.argv.slice(2)
appendFileSync(process.env.SCOPE_TEST_CALLS, JSON.stringify(args) + '\\n')
if (args.includes('doctor')) console.log(JSON.stringify({ ok: true }))
else if (args.includes('track-add')) {
  if (!args.includes('--root-pid-namespace')) process.exit(2)
  console.log(JSON.stringify({ ok: true, operation_state: 'active', trace_id: 17 }))
} else process.exit(2)
`, { mode: 0o755 })
      const env = { ...process.env, DSH_HOME: directory, CENSORSCOPE_CTL: ctl, SCOPE_TEST_CALLS: calls }
      delete env.AGENTCENSOR_SESSION_ID
      delete env.CENSORSCOPE_SESSION_ID
      const result = spawnSync(process.execPath, [import.meta.filename, '--host'], {
        env, encoding: 'utf8', timeout: 10000,
      })
      assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stderr}`)
      const invocations = (await readFile(calls, 'utf8')).trim().split('\n').map(line => JSON.parse(line))
      const registration = invocations.find(args => args.includes('track-add'))
      assert.ok(registration)
      assert.equal(registration[registration.indexOf('--root-pid-namespace') + 1], await readlink('/proc/self/ns/pid'))
      const trace = JSON.parse(await readFile(join(directory, '.censorscope', 'trace.json'), 'utf8'))
      assert.equal(registration[registration.indexOf('--root-pid') + 1], String(trace.rootPid))
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })
}
