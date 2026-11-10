import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout } from 'node:timers/promises'
import test from 'node:test'
import { registerSpanEmitter } from '../lib/index.mjs'

test('parallel tool spans keep child identities, real dispatch time and failure status', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'scope-call-spans-'))
  const previous = { ...process.env }
  t.after(async () => {
    for (const key of Object.keys(process.env)) if (!(key in previous)) delete process.env[key]
    Object.assign(process.env, previous)
    await rm(directory, { recursive: true, force: true })
  })
  const calls = join(directory, 'calls.jsonl')
  const gate = join(directory, 'release-starts')
  const ctl = join(directory, 'ctl.mjs')
  await writeFile(ctl, `#!${process.execPath}
import { appendFileSync, existsSync } from 'node:fs'
import { setTimeout } from 'node:timers/promises'
const args = process.argv.slice(2)
appendFileSync(${JSON.stringify(calls)}, JSON.stringify(args) + '\\n')
if (args.includes('call-start')) while (!existsSync(${JSON.stringify(gate)})) await setTimeout(10)
console.log(JSON.stringify({ok:true}))
`, { mode: 0o755 })
  await mkdir(join(directory, '.censorscope'))
  await writeFile(join(directory, '.censorscope/trace.json'), JSON.stringify({ traceId: '17' }))
  Object.assign(process.env, {
    DSH_HOME: directory, CENSORSCOPE_CTL: ctl,
    CENSORSCOPE_SESSION_ID: 'parent', AGENTCENSOR_SESSION_ID: 'parent',
  })
  const hooks = new Map()
  registerSpanEmitter({ on: (name, callback) => hooks.set(name, callback) })
  const execute = hooks.get('tools/execute')
  const executions = [
    { callId: 'read-a', agent: { session: { id: 'child-a' } } },
    { callId: 'write-b', agent: { session: { id: 'child-b' } } },
    { callId: 'error-c', agent: { session: { id: 'child-c' } } },
    { callId: 'unknown' },
  ]
  const failure = new Error('aborted')
  const result = { isError: false, content: [] }
  const work = executions.map((exec, index) => execute(exec, async () => {
    await setTimeout(30)
    if (index === 1) throw failure
    return index === 2 ? { isError: true } : result
  }))
  const outcomes = await Promise.allSettled(work)
  assert.equal(outcomes[0].value, result)
  assert.equal(outcomes[1].reason, failure)
  const records = async () => {
    try { return (await readFile(calls, 'utf8')).trim().split('\n').map(line => JSON.parse(line)) }
    catch (error) { if (error.code === 'ENOENT') return []; throw error }
  }
  const waitFor = async predicate => {
    for (let attempt = 0; attempt < 200; attempt++) {
      const rows = await records()
      if (predicate(rows)) return rows
      await setTimeout(10)
    }
    assert.fail('span reporting did not finish')
  }
  const starts = await waitFor(rows => rows.length === 4)
  assert.ok(starts.every(row => row.includes('call-start')), 'tool completion must not await daemon acknowledgement')
  await writeFile(gate, '')
  const rows = await waitFor(rows => rows.filter(row => row.includes('call-end')).length === 4)
  const argument = (row, flag) => row.includes(flag) ? row[row.indexOf(flag) + 1] : undefined
  for (const [index, exec] of executions.entries()) {
    const own = rows.filter(row => argument(row, '--call-id') === exec.callId)
    assert.equal(own.length, 2)
    assert.ok(own[0].includes('call-start'))
    assert.ok(own[1].includes('call-end'))
    assert.equal(argument(own[0], '--session-id'), exec.agent?.session.id)
    assert.equal(argument(own[1], '--session-id'), exec.agent?.session.id)
    assert.ok(BigInt(argument(own[1], '--ended-at')) - BigInt(argument(own[0], '--started-at')) >= 20_000_000n)
    assert.equal(argument(own[1], '--status'), index === 1 || index === 2 ? 'error' : 'success')
    hooks.get('tools/result')(exec, { isError: false })
    hooks.get('session/event')(exec.agent?.session, {
      type: 'tool/result', time: Date.now() + 1000, data: { callId: exec.callId },
    })
  }
  hooks.get('session/event')({ id: 'mirrored' }, { type: 'tool/call', data: { callId: 'remote' } })
  hooks.get('session/event')({ id: 'mirrored' }, { type: 'tool/result', data: { callId: 'remote' } })
  await setTimeout(50)
  assert.equal((await records()).length, 8, 'late and mirrored events must not create duplicate spans')
})
