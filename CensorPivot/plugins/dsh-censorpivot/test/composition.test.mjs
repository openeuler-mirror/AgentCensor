import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

async function manifest(path = 'package.json') {
  return JSON.parse(await readFile(resolve(root, path), 'utf8'))
}

test('publishes one user-facing web bundle', async () => {
  const pkg = await manifest()
  assert.equal(pkg.name, '@agentcensor/censorpivot')
  assert.equal(pkg.dsh.bundle.patch, './cordis.patch.yml')
  assert.equal(pkg.dsh.client, undefined)
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
    "name: 'censorscope-ui'",
  ]) assert.match(patch, new RegExp(expected.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  assert.match(patch, /id: agent-loop\n  inject: \[censorguardReady\]\n  disabled: true/)
})

test('headless companion contains only worker-side integrations', async () => {
  const pkg = await manifest('headless/package.json')
  const patch = await readFile(resolve(root, 'headless/cordis.patch.yml'), 'utf8')
  assert.equal(pkg.name, '@agentcensor/censorpivot-headless')
  assert.match(patch, /name: '@censorfs\/event-exporter'/)
  assert.match(patch, /name: 'censorscope-host'/)
  assert.doesNotMatch(patch, /censorguard|session-proxy|censorscope-ui/)
})
