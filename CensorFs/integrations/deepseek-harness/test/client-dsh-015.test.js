import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import vm from 'node:vm'

async function loadClientBundle() {
  const source = await readFile(new URL('../client/index.js', import.meta.url), 'utf8')
  let plugin
  const context = vm.createContext({
    window: {
      __ModuleLoader__: {
        load(definition) {
          assert.equal(definition.id, '@censorfs/deepseek-harness')
          plugin = definition.factory((name) => {
            assert.equal(name, 'react')
            return {
              createElement() {},
              useEffect() {},
              useState() {},
            }
          })
        },
      },
    },
  })
  vm.runInContext(source, context, { filename: 'censorfs-client.js' })
  return plugin
}

test('DSH 0.1.5 client activates through uiConversation', async () => {
  const plugin = await loadClientBundle()
  assert.deepEqual(
    [...plugin.inject],
    ['uiConversation', 'slots', 'remote', 'remote.commands', 'sessions'],
  )

  const events = []
  const views = []
  const slots = []
  const ctx = {
    uiConversation: {
      events: { register: (definition) => events.push(definition) },
      views: { register: (definition) => views.push(definition) },
    },
    slots: {
      inject(_name, activate) {
        activate()
      },
      register(options, component) {
        slots.push({ options, component })
        return () => {}
      },
    },
    remote: { commands: { execute() {} } },
    sessions: { open() {} },
  }

  plugin.apply(ctx)

  assert.deepEqual(events.map(({ kind }) => kind), [
    'branch-explore',
    'branch-explore-worlds',
    'subagent-graph',
  ])
  assert.deepEqual(views.map(({ target }) => target), ['subagents', 'worlds'])
  assert.deepEqual(slots.map(({ options }) => options.name), [
    'conversation.chat.node',
    'conversation.view',
  ])
})
