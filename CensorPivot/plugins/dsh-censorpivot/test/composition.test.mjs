import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import vm from 'node:vm'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

async function manifest(path = 'package.json') {
  return JSON.parse(await readFile(resolve(root, path), 'utf8'))
}

test('publishes one user-facing web bundle', async () => {
  const pkg = await manifest()
  assert.equal(pkg.name, '@agentcensor/censorpivot')
  assert.equal(pkg.dsh.bundle.patch, './cordis.patch.yml')
  assert.deepEqual(pkg.dsh.client.inject, [
    '@deepseek-ai/dsh-client-runtime',
    '@deepseek-ai/dsh-client-ui-settings',
    '@deepseek-ai/dsh-client-ui-conversation',
  ])
})

test('registers the browser client with its package identity', async () => {
  const source = await readFile(resolve(root, 'lib/client.js'), 'utf8')
  let registration
  vm.runInNewContext(source, {
    window: { __ModuleLoader__: { load: (value) => { registration = value } } },
    localStorage: { getItem: () => null, setItem: () => undefined },
    document: {},
    setTimeout: () => undefined,
  })
  assert.equal(registration.id, '@agentcensor/censorpivot')
  const client = registration.factory((id) => {
    assert.equal(id, 'react')
    return { createElement: () => undefined, useState: () => undefined, useSyncExternalStore: () => undefined }
  })
  assert.equal(client.name, 'censorpivot-ui')
  assert.equal(typeof client.apply, 'function')
  assert.match(source, /\/censorpivot\/call\?session=/)
  assert.match(source, /censorscoped 原始系统调用/)
  assert.match(source, /会话工具调用观测/)
  assert.match(source, /user\/message/)
  assert.match(source, /pivot-scope-scroll/)
  assert.match(source, /isRuntimeContext/)
  assert.match(source, /closePivotSettings/)
  assert.match(source, /censorguard-read/)
  assert.match(source, /audit\/snapshot/)
})

test('web patch composes all three existing modules', async () => {
  const patch = await readFile(resolve(root, 'cordis.patch.yml'), 'utf8')
  for (const expected of [
    "name: '@censorfs/deepseek-harness'",
    "name: '@censorguard/dsh/bootstrap'",
    "name: '@censorguard/dsh/host'",
    "name: '@censorguard/dsh'",
    "name: 'censorscope-host'",
    "name: 'agentcensor-session-proxy'",
  ]) assert.match(patch, new RegExp(expected.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  assert.doesNotMatch(patch, /name: 'censorscope-ui'/)
  assert.match(patch, /id: agent-loop\r?\n  inject: \[censorguardReady\]\r?\n  disabled: true/)
  assert.match(patch, /blockOnFailure: true/)
  assert.match(patch, /id: censorfs-parallel-worlds\r?\n  inject: \[tools, commands, subagents, sessions, censorguardReady\]/)
  assert.match(patch, /id: agentcensor-session-proxy\r?\n  inject: \[agents, sessions, sessionPersistence, sessionProjections, censorguardReady\]/)
  assert.match(patch, /id: skill\r?\n  disabled: false/)
  assert.doesNotMatch(patch, /id: ui-trajectory\r?\n  disabled: true/)
})

test('headless companion contains only worker-side integrations', async () => {
  const pkg = await manifest('headless/package.json')
  const patch = await readFile(resolve(root, 'headless/cordis.patch.yml'), 'utf8')
  const source = await readFile(resolve(root, 'headless/lib/index.mjs'), 'utf8')
  assert.equal(pkg.name, '@agentcensor/censorpivot-headless')
  assert.match(patch, /name: '@censorfs\/deepseek-harness'/)
  assert.match(patch, /name: '@censorfs\/event-exporter'/)
  assert.match(patch, /name: 'censorscope-host'/)
  assert.doesNotMatch(patch, /censorguard|session-proxy|censorscope-ui/)
  assert.match(source, /AGENTCENSOR_SESSION_ID/)
  assert.match(source, /track-add/)
  assert.match(source, /--trace-id/)
  assert.match(source, /censorscopectl-dsh/)
})

test('source CLI shares the DSH Scope module and Guard has no retired client dependency', async () => {
  const scopeRoot = resolve(root, '../../../CensorScope/plugins/agentcensor-session-proxy')
  const guardRoot = resolve(root, '../../../CensorGuard/plugins/dsh-censorguard')
  for (const file of ['lib/index.mjs', 'lib/interaction-bridge.mjs']) {
    const source = await readFile(resolve(scopeRoot, file), 'utf8')
    assert.match(source, /@deepseek-ai\/dsh-scope'/)
    assert.match(source, /@deepseek-ai\/dsh-scope\/src\/index\.ts/)
  }
  const proxy = await readFile(resolve(scopeRoot, 'lib/index.mjs'), 'utf8')
  assert.match(proxy, /const dshLauncher = await resolveDshWorkerLauncher\(\)/)
  assert.match(proxy, /persistence\.open\(id, 'write'\)/)
  assert.match(proxy, /persistence\.create\(session\.header, \{/)
  assert.match(proxy, /inheritedEventCount: options\.inheritedEventCount/)
  assert.doesNotMatch(proxy, /persistence\.prepare\(/)
  const guard = JSON.parse(await readFile(resolve(guardRoot, 'package.json'), 'utf8'))
  assert.ok(!guard.dsh.client.inject.includes('@deepseek-ai/dsh-client-runtime'))
})
