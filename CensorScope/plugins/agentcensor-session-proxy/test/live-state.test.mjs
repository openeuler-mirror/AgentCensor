import assert from 'node:assert/strict'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import test from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { agentEvents, installModelSelection } from '@deepseek-ai/dsh-agent'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import { Session } from '@deepseek-ai/dsh-session'
import { AssistantStreamBridge } from '../lib/assistant-stream-bridge.mjs'
import { controlLoop } from '../lib/worker.mjs'

const { SessionAssistantStreamAccumulator } = await import(pathToFileURL(join(
  process.env.DSH_ROOT, 'packages/api/session-controller/src/assistant-stream.ts',
)).href)

test('worker model changes affect native prompt assembly and the next request together', async t => {
  const ctx = new Context()
  await ctx.plugin(SystemPrompt)
  t.after(() => ctx.fiber.dispose())
  const session = Session.create('model-switch')
  const agent = { session, ctx, cancel() {} }
  const selection = { current: { provider: 'alpha', model: 'a' }, assembled: undefined }
  installModelSelection(ctx, selection)
  const request = () => agentEvents(ctx, agent).waterfall(
    'agent/request', { turn: 1, step: 1, signal: new AbortController().signal },
    async () => ({ provider: 'fallback', model: 'fallback', reasoningEffort: 'max' }),
  )
  const configFrame = data => JSON.stringify({ type: 'host-session-event', event: { type: 'model/selection', data } })
  async function* frames() {
    yield configFrame({ provider: 'beta', model: 'b', reasoningEffort: 'high' })
    assert.deepEqual((await ctx.systemPrompt.assemble()).variables, { provider: 'beta', model: 'b' })
    assert.deepEqual(await request(), { provider: 'beta', model: 'b', reasoningEffort: 'high' })
    yield configFrame({ provider: 'gamma', model: 'c' })
    assert.deepEqual(await request(), { provider: 'beta', model: 'b', reasoningEffort: 'high' })
    await ctx.systemPrompt.assemble()
    assert.deepEqual(await request(), { provider: 'gamma', model: 'c' })
  }
  await controlLoop(agent, { async flush() {} }, { close() {} }, frames(), { close() {} }, selection)
  assert.equal(session.snapshotEvents().filter(event => event.type === 'model/selection').length, 2)
})

test('live frames survive reconnect and worker restart using the native accumulator', () => {
  const published = []
  const accumulator = new SessionAssistantStreamAccumulator()
  const bridge = new AssistantStreamBridge(frame => {
    published.push(frame)
    accumulator.accept(frame, 10)
  })
  const start = { type: 'start', attemptId: 'session:1', revision: 1, turn: 1, step: 1 }
  const chunk = { type: 'chunk', attemptId: 'session:1', revision: 2, index: 0, time: 20,
    chunk: { type: 'text-delta', index: 0, text: 'hello' } }
  bridge.startWorker()
  bridge.accept(start)
  bridge.accept(chunk)
  assert.equal(accumulator.snapshot().activeAttempt.stream[0].texts[0], 'hello')
  const firstId = accumulator.snapshot().activeAttempt.attemptId
  bridge.startWorker()
  assert.equal(accumulator.snapshot().activeAttempt, undefined)
  bridge.accept(start)
  bridge.accept(chunk)
  assert.notEqual(accumulator.snapshot().activeAttempt.attemptId, firstId)
  bridge.accept({ type: 'end', attemptId: 'session:1', revision: 3, index: 1,
    outcome: { kind: 'committed', eventType: 'assistant/message', seq: 8 } }, seq => seq === 8 ? 12 : undefined)
  assert.equal(published.at(-1).outcome.seq, 12)
  assert.equal(accumulator.snapshot().activeAttempt, undefined)
  assert.deepEqual(published.map(frame => frame.revision), [1, 2, 3, 4, 5, 6])
})

test('gaps and unmirrored settlements fail instead of publishing a false completion', () => {
  const frames = []
  const bridge = new AssistantStreamBridge(frame => frames.push(frame))
  bridge.startWorker()
  const start = { type: 'start', attemptId: 's:1', revision: 1, turn: 1, step: 1 }
  bridge.accept(start)
  assert.throws(() => bridge.accept({ type: 'chunk', revision: 3 }), /revision gap/)
  assert.throws(() => bridge.accept({ type: 'end', attemptId: 's:1', revision: 2, index: 0,
    outcome: { kind: 'committed', eventType: 'assistant/message', seq: 4 } }, () => undefined), /not mirrored/)
  bridge.close()
  assert.equal(frames.at(-1).outcome.kind, 'abandoned')
})
