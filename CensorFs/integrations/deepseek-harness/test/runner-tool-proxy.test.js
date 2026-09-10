import test from 'node:test'
import assert from 'node:assert/strict'
import { registerRunnerToolProxy } from '../src/runner-tool-proxy.js'

function setup() {
  let listener
  const ctx = {
    on(event, callback) {
      assert.equal(event, 'tools/execute')
      listener = callback
    },
    get() { return undefined },
  }
  const calls = []
  const agent = { session: { id: 'session-a' } }
  const record = {
    client: {
      async call(method, args, signal) {
        calls.push({ method, args, signal })
        return { path: '/workspace/a.txt', offset: 1, lines: [], totalLines: 0 }
      },
    },
  }
  const manager = { get(value) { return value === agent ? record : undefined } }
  registerRunnerToolProxy(ctx, manager)
  return { agent, calls, listener }
}

test('tool proxy short-circuits runner-required tools for the exact Agent', async () => {
  const { agent, calls, listener } = setup()
  const signal = new AbortController().signal
  let nextCalled = false
  const result = await listener({
    agent,
    name: 'read',
    arguments: { file_path: '/workspace/a.txt' },
    signal,
  }, async () => { nextCalled = true })
  assert.equal(nextCalled, false)
  assert.deepEqual(calls, [{ method: 'fs.read', args: { file_path: '/workspace/a.txt' }, signal }])
  assert.equal(result.isError, false)
  assert.equal(result.value.path, '/workspace/a.txt')
})

test('tool proxy delegates host-only tools and unbound Agents', async () => {
  const { agent, listener } = setup()
  const host = { isError: false, value: 'host', content: [] }
  assert.equal(await listener({ agent, name: 'web_search', arguments: {}, signal: new AbortController().signal }, async () => host), host)
  assert.equal(await listener({ agent: {}, name: 'read', arguments: {}, signal: new AbortController().signal }, async () => host), host)
})

test('tool proxy fails closed for unsupported local execution and escalation', async () => {
  const { agent, listener } = setup()
  const signal = new AbortController().signal
  await assert.rejects(() => listener({ agent, name: 'lsp', arguments: {}, signal }, async () => undefined), /no CensorFS execution-world adapter/u)
  await assert.rejects(() => listener({ agent, name: 'unknown_file_tool', arguments: {}, signal }, async () => undefined), /no declared CensorFS execution-world adapter/u)
  await assert.rejects(() => listener({
    agent,
    name: 'bash',
    arguments: { command: 'true', sandbox_permissions: 'danger-full-access', justification: 'test' },
    signal,
  }, async () => undefined), /escalation is unavailable/u)
})
