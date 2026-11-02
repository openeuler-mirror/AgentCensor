import assert from 'node:assert/strict'
import test from 'node:test'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { Context } from '@deepseek-ai/cordis'
import { Session } from '@deepseek-ai/dsh-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'
import { inboxProjectionDefinition, ProxyInbox } from '../lib/inbox.mjs'
import { controlLoop } from '../lib/worker.mjs'
import { nativeHarness } from './native-harness.mjs'

const message = text => createUserMessage({ content: [{ type: 'text', text }], source: { kind: 'user' } })
const frame = value => JSON.stringify(value)
const sessions = { async flush() {} }

test('worker delivers input directly to native turn/step inboxes without serializing whole turns', async t => {
  const gate = Promise.withResolvers()
  const { ctx, agent } = await nativeHarness(t)
  let injected = false
  let steered = false
  const insertionTargets = []
  ctx.on('session/event', (_session, event) => {
    if (event.type === 'agent/inbox/spliced' && event.data.inserted.length) insertionTargets.push(event.data.target)
  })
  ctx.on('agent/pre-step', async (_payload, next) => { await gate.promise; return next() })
  async function* frames() {
    yield frame({ type: 'message', message: message('context'), target: 'next-step', wakeup: false })
    assert.equal(agent.status, 'idle')
    assert.equal(agent.inbox.nextStep.length, 1)
    injected = true
    yield frame({ type: 'message', message: message('first'), target: 'next-turn', wakeup: true })
    yield frame({ type: 'message', message: message('steering'), target: 'next-step', wakeup: true })
    yield frame({ type: 'message', message: message('follow-up'), target: 'next-turn', wakeup: true })
    assert.equal(agent.inbox.nextStep.at(-1).content[0].text, 'steering')
    assert.equal(agent.inbox.nextTurn.at(-1).content[0].text, 'follow-up')
    steered = true
    gate.resolve()
    await agent.whenIdle()
    yield frame({ type: 'shutdown' })
  }
  try {
    await controlLoop(agent, sessions, { close() {} }, frames(), { close() {} })
  } finally { gate.resolve() }
  assert.ok(injected && steered)
  assert.deepEqual(insertionTargets.slice(0, 4), ['next-step', 'next-turn', 'next-step', 'next-turn'])
})

test('shutdown cancels native activity before waiting for pending work and closes human asks', async t => {
  const started = Promise.withResolvers()
  const { agent } = await nativeHarness(t, async ({ signal }) => {
    started.resolve()
    await new Promise((resolve, reject) => {
      if (signal.aborted) return reject(new Error('aborted'))
      signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true })
    })
  })
  let asksClosed = false
  async function* frames() {
    yield frame({ type: 'message', message: message('wait for answer') })
    await started.promise
    yield frame({ type: 'shutdown' })
  }
  await controlLoop(agent, sessions, { close() { asksClosed = true } }, frames(), { close() {} })
  assert.ok(asksClosed)
  assert.equal(agent.status, 'idle')
  const end = agent.session.snapshotEvents().find(event => event.type === 'turn/end')
  assert.equal(end.data.reason.kind, 'aborted')
  assert.equal(end.data.reason.reason.kind, 'disposed')
})

test('host pending projection and queue edits round trip through the native worker inbox', async t => {
  const { ctx, agent } = await nativeHarness(t)
  const host = new Context()
  await host.plugin(SessionProjectionRegistry)
  host.sessionProjections.register(inboxProjectionDefinition)
  t.after(() => host.fiber.dispose())
  const mirrored = Session.create('host')
  const commands = []
  const inbox = new ProxyInbox(host.sessionProjections, mirrored, command => commands.push(command))
  ctx.on('session/event', (session, event) => {
    if (session === agent.session) mirrored.append(event.type, event.data)
  })
  const a = message('queued a')
  const b = message('queued b')
  const edited = { ...a, content: [{ type: 'text', text: 'edited' }] }
  async function* frames() {
    yield frame({ type: 'message', message: a, target: 'next-step', wakeup: false })
    yield frame({ type: 'message', message: b, target: 'next-step', wakeup: false })
    assert.equal(inbox.hasPending, true)
    assert.deepEqual(inbox.nextStep.map(value => value.id), [a.id, b.id])
    assert.equal(inbox.replace(a.id, edited), true)
    yield frame({ type: 'inbox', ...commands.shift() })
    assert.deepEqual(agent.inbox.nextStep, inbox.nextStep)
    assert.equal(inbox.nextStep[0].content[0].text, 'edited')
    assert.equal(inbox.remove(b.id), true)
    yield frame({ type: 'inbox', ...commands.shift() })
    assert.deepEqual(agent.inbox.nextStep, inbox.nextStep)
    assert.equal(inbox.remove(b.id), false)
    inbox.clear()
    yield frame({ type: 'inbox', ...commands.shift() })
    assert.equal(inbox.hasPending, false)
    yield frame({ type: 'shutdown' })
  }
  await controlLoop(agent, sessions, { close() {} }, frames(), { close() {} })
})
