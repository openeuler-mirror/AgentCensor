import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import test from 'node:test'
import { installModelSelection } from '@deepseek-ai/dsh-agent'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { AssistantStreamBridge } from '../lib/assistant-stream-bridge.mjs'
import { controlLoop } from '../lib/worker.mjs'
import { nativeHarness } from './native-harness.mjs'

const sourceAccumulator = join(process.env.DSH_ROOT, 'packages/api/session-controller/src/assistant-stream.ts')
const controllerRoot = dirname(fileURLToPath(import.meta.resolve('@deepseek-ai/dsh-api-session-controller/package.json')))
const accumulatorPath = existsSync(sourceAccumulator)
  ? sourceAccumulator
  : join(controllerRoot, 'lib/types/assistant-stream.js')
const { SessionAssistantStreamAccumulator } = await import(pathToFileURL(accumulatorPath).href)

test('worker model changes use the native selection for each request and preserve an in-flight request', async t => {
  const started = Promise.withResolvers()
  const release = Promise.withResolvers()
  const requests = []
  const { ctx, agent, adapter } = await nativeHarness(t, async options => {
    requests.push({ provider: options.provider, model: options.model, reasoningEffort: options.reasoningEffort })
    if (requests.length === 1) {
      started.resolve()
      await release.promise
    }
  })
  ctx.llm.registerAdapter(['beta', 'gamma'], adapter)
  installModelSelection(agent.ctx)
  const configFrame = data => JSON.stringify({ type: 'host-session-event', event: { type: 'model/selection', data } })
  const messageFrame = text => JSON.stringify({
    type: 'message',
    message: createUserMessage({ content: [{ type: 'text', text }], source: { kind: 'user' } }),
  })
  async function* frames() {
    yield configFrame({ provider: 'beta', model: 'b', reasoningEffort: 'high' })
    assert.deepEqual(agent.model.current(), { provider: 'beta', model: 'b', reasoningEffort: 'high' })
    yield messageFrame('first request')
    await started.promise
    yield configFrame({ provider: 'gamma', model: 'c' })
    assert.deepEqual(agent.model.current(), { provider: 'gamma', model: 'c' })
    assert.deepEqual(requests, [{ provider: 'beta', model: 'b', reasoningEffort: 'high' }])
    release.resolve()
    await agent.whenIdle()
    yield messageFrame('second request')
    await agent.whenIdle()
    yield JSON.stringify({ type: 'shutdown' })
  }
  try {
    await controlLoop(agent, { async flush() {} }, { close() {} }, frames(), { close() {} })
  } finally {
    release.resolve()
  }
  assert.deepEqual(requests, [
    { provider: 'beta', model: 'b', reasoningEffort: 'high' },
    { provider: 'gamma', model: 'c', reasoningEffort: undefined },
  ])
  assert.equal(agent.session.snapshotEvents().filter(event => event.type === 'model/selection').length, 2)
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
