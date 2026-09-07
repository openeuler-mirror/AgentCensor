import test from 'node:test'
import assert from 'node:assert/strict'

import { CombinedBranchExploreRuntime } from '../src/combined-runtime.js'
import { resolveEnabledTracks } from '../src/track-config.js'

function minimalConfig(overrides = {}) {
  return {
    socket: '/run/censorfs/control.sock',
    mounterCommand: 'censorfs-mounter',
    controlPlaneCwd: '/root',
    childCommand: 'dsh-jsonrpc-agent',
    provider: 'deepseek-official',
    model: 'deepseek-v4-flash',
    validationProfiles: { code: [] },
    ...overrides,
  }
}

function fakeRuntime(tag) {
  const calls = []
  return {
    calls,
    tag,
    explore: (...args) => { calls.push(['explore', ...args]); return { track: tag, op: 'explore' } },
    exploreInProcess: (...args) => { calls.push(['exploreInProcess', ...args]); return { track: tag, op: 'exploreInProcess' } },
    state: (agent, runId) => { calls.push(['state', runId]); return { track: tag, runId } },
    validate: () => ({ track: tag, op: 'validate' }),
    publish: () => ({ track: tag, op: 'publish' }),
    abort: () => ({ track: tag, op: 'abort' }),
    preview: () => ({ track: tag, op: 'preview' }),
    updatePolicy: () => ({ track: tag, op: 'updatePolicy' }),
    doctor: () => ({ track: tag, op: 'doctor' }),
    resolveRootSession: () => 'root',
    clearSubagentGraph: () => 'cleared',
    subagentTree: () => ({ track: tag }),
    subagentOverview: () => ({ track: tag }),
    injectSysActivity: () => 'injected',
    trackSubagentStart: (info) => { calls.push(['trackSubagentStart', info]) },
    trackSubagentEnd: (info) => { calls.push(['trackSubagentEnd', info]) },
    trackSubagentActivity: (session, event) => { calls.push(['trackSubagentActivity', event?.type]) },
    dispose: async () => { calls.push(['dispose']) },
  }
}

function agentWithRun(runId, mode) {
  return { session: { events: [{ type: 'exploration-started', data: { runId, mode } }] } }
}

test('resolveEnabledTracks: both tracks by default', () => {
  assert.deepEqual(resolveEnabledTracks(minimalConfig()), { fuse: true, runner: true })
})

test('resolveEnabledTracks: inProcessOnly disables fuse, fuseOnly disables runner', () => {
  assert.deepEqual(resolveEnabledTracks(minimalConfig({ inProcessOnly: true })), { fuse: false, runner: true })
  assert.deepEqual(resolveEnabledTracks(minimalConfig({ fuseOnly: true })), { fuse: true, runner: false })
})

test('normalizeConfig rejects mutually exclusive single-track flags', () => {
  assert.throws(
    () => resolveEnabledTracks({ inProcessOnly: true, fuseOnly: true }),
    /mutually exclusive/,
  )
})

test('resolveEnabledTracks is dependency-free and flag-driven only', () => {
  assert.deepEqual(resolveEnabledTracks(undefined), { fuse: true, runner: true })
  assert.deepEqual(resolveEnabledTracks({ inProcessOnly: '1' }), { fuse: true, runner: true })
  assert.deepEqual(resolveEnabledTracks({ inProcessOnly: 1 }), { fuse: true, runner: true })
  assert.deepEqual(resolveEnabledTracks({ fuseOnly: true }), { fuse: true, runner: false })
})

test('combined mis-wiring throws: enabled track missing / disabled track constructed', () => {
  const fuse = fakeRuntime('fuse')
  const runner = fakeRuntime('runner')
  assert.throws(() => new CombinedBranchExploreRuntime(undefined, runner, minimalConfig({})), /mis-wired/)
  assert.throws(() => new CombinedBranchExploreRuntime(fuse, runner, minimalConfig({ inProcessOnly: true })), /mis-wired/)
  assert.throws(() => new CombinedBranchExploreRuntime(fuse, runner, minimalConfig({ fuseOnly: true })), /mis-wired/)
})

test('explore/exploreInProcess gate on enabled tracks and delegate on enabled ones', () => {
  const runnerOnly = new CombinedBranchExploreRuntime(undefined, fakeRuntime('runner'), minimalConfig({ inProcessOnly: true }))
  assert.throws(() => runnerOnly.explore({}, {}), /branch_explore is unavailable/)
  const agent = agentWithRun('r', 'in-process')
  assert.equal(runnerOnly.exploreInProcess(agent, {}).op, 'exploreInProcess')

  const fuseOnly = new CombinedBranchExploreRuntime(fakeRuntime('fuse'), undefined, minimalConfig({ fuseOnly: true }))
  assert.throws(() => fuseOnly.exploreInProcess({}, {}), /in-process track.*disabled|branch_explore_inprocess/i)
  assert.equal(fuseOnly.explore({}, {}).op, 'explore')
})

test('run-scoped operations route by recorded mode and fail closed cross-track', () => {
  const fuse = fakeRuntime('fuse')
  const runner = fakeRuntime('runner')
  const combined = new CombinedBranchExploreRuntime(fuse, runner, minimalConfig({}))

  assert.equal(combined.state(agentWithRun('r1', 'in-process'), 'r1').track, 'runner')
  assert.equal(combined.publish(agentWithRun('r2', 'external'), 'r2', 'v1', false).track, 'fuse')
  assert.equal(combined.preview(agentWithRun('r3', 'in-process'), 'r3', 'v1').track, 'runner')
  assert.equal(combined.validate(agentWithRun('r4', 'external'), 'r4', 'v1').track, 'fuse')
  assert.equal(combined.abort(agentWithRun('r5', 'in-process'), 'r5', 'v1').track, 'runner')

  const runnerOnly = new CombinedBranchExploreRuntime(undefined, runner, minimalConfig({ inProcessOnly: true }))
  assert.throws(() => runnerOnly.state(agentWithRun('r6', 'external'), 'r6'), /FUSE track.*disabled/i)
  assert.throws(() => runnerOnly.updatePolicy(agentWithRun('r7', 'in-process'), 'r7', 'v1', {}), /in-process runs/)
  assert.equal(runnerOnly.doctor().op, 'doctor')

  const fuseOnly = new CombinedBranchExploreRuntime(fuse, undefined, minimalConfig({ fuseOnly: true }))
  assert.throws(() => fuseOnly.state(agentWithRun('r8', 'in-process'), 'r8'), /in-process track.*disabled|branch_explore_inprocess/i)
  assert.throws(() => fuseOnly.doctor(), /in-process track.*disabled|branch_explore_inprocess/i)
  assert.throws(() => fuseOnly.preview(agentWithRun('r9', 'in-process'), 'r9', 'v1'), /in-process track.*disabled|branch_explore_inprocess/i)
  assert.equal(fuseOnly.subagentTree().track, 'fuse')
})

test('unknown runs keep delegating to the FUSE runtime state lookup', () => {
  const fuse = fakeRuntime('fuse')
  const combined = new CombinedBranchExploreRuntime(fuse, fakeRuntime('runner'), minimalConfig({}))
  assert.equal(combined.state({ session: { events: [] } }, 'nope').track, 'fuse')
})

test('tracking fan-out and dispose tolerate missing tracks', async () => {
  const fuse = fakeRuntime('fuse')
  const combined = new CombinedBranchExploreRuntime(fuse, undefined, minimalConfig({ fuseOnly: true }))
  combined.trackSubagentStart({ id: 's1' })
  combined.trackSubagentActivity({ session: {} }, { type: 'x' })
  assert.deepEqual(fuse.calls[0], ['trackSubagentStart', { id: 's1' }])
  await combined.dispose()
  assert.deepEqual(fuse.calls.at(-1), ['dispose'])

  const both = new CombinedBranchExploreRuntime(fakeRuntime('fuse'), fakeRuntime('runner'), minimalConfig({}))
  assert.doesNotThrow(() => both.trackSubagentEnd({ id: 's2' }))
})
