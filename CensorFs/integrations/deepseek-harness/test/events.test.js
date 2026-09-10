import test from 'node:test'
import assert from 'node:assert/strict'
import { EVENT_TYPES, foldExploration, isCensorFsEvent, rankVariants } from '../src/events.js'

test('durable events rebuild a ranked arena after refresh', () => {
  const events = [
    { type: 'exploration-started', data: { runId: 'r1', task: 'fix', branch: 'main', expectedHead: { generation_id: 'g0', head_seq: 1 }, validationProfile: 'code' } },
    { type: 'variant-running', data: { runId: 'r1', variantId: 'small', label: 'Small', ticketId: 't1' } },
    { type: 'variant-running', data: { runId: 'r1', variantId: 'large', label: 'Large', ticketId: 't2' } },
    { type: 'variant-prepared', data: { runId: 'r1', variantId: 'small', candidate: { candidate_id: 'c1' }, generation: { generation_id: 'g1' }, validation: { requiredPassed: true }, pathDiff: [{ path: '/a' }] } },
    { type: 'variant-prepared', data: { runId: 'r1', variantId: 'large', candidate: { candidate_id: 'c2' }, generation: { generation_id: 'g2' }, validation: { requiredPassed: false }, pathDiff: [{ path: '/a' }, { path: '/b' }] } },
    { type: 'ranking-ready', data: { runId: 'r1', ranking: [{ rank: 1, variantId: 'small' }, { rank: 2, variantId: 'large' }] } },
    { type: 'exploration-ended', data: { runId: 'r1', status: 'ranked' } },
  ]
  const state = foldExploration(events, 'r1')
  assert.equal(state.status, 'ranked')
  assert.equal(state.variants.small.status, 'prepared')
  assert.equal(state.variants.large.status, 'validation-failed')
  assert.deepEqual(state.ranking.map((entry) => entry.variantId), ['small', 'large'])
})

test('ranking gates required validation before change size and duration', () => {
  const ranking = rankVariants([
    { variantId: 'failed-fast', candidate: {}, validation: { requiredPassed: false, checks: [{ passed: false }] }, pathDiff: [], durationMs: 1 },
    { variantId: 'bigger', candidate: {}, validation: { requiredPassed: true, checks: [{ passed: true }] }, pathDiff: [{}, {}], durationMs: 20 },
    { variantId: 'smaller', candidate: {}, validation: { requiredPassed: true, checks: [{ passed: true }] }, pathDiff: [{}], durationMs: 50 },
  ])
  assert.deepEqual(ranking.map((entry) => entry.variantId), ['smaller', 'bigger'])
})

test('isolation-fallback folds into the variant, downgrades run-level isolation, and survives later events', () => {
  const events = [
    { type: 'exploration-started', data: { runId: 'r1', task: 'fix', branch: 'main', expectedHead: { generation_id: 'g0', head_seq: 1 }, validationProfile: 'code', isolation: { requestedMode: 'auto', minimumLevel: 'process', effectiveLevel: 'resource', cgroupEnabled: true, cgroupRoot: '/sys/fs/cgroup/censorfs-runners', warnings: [] } } },
    { type: 'variant-running', data: { runId: 'r1', variantId: 'small', label: 'Small', ticketId: 't1' } },
    { type: 'isolation-fallback', data: { runId: 'r1', variantId: 'small', fromLevel: 'resource', originalFromLevel: 'resource', toLevel: 'process', warning: 'cgroup-backed Runner launch failed; auto fallback to process isolation for small' } },
    { type: 'variant-failed', data: { runId: 'r1', variantId: 'small', label: 'Small', error: 'second attempt also failed' } },
    { type: 'exploration-ended', data: { runId: 'r1', status: 'ranked' } },
  ]
  const state = foldExploration(events, 'r1')
  // The run-level isolation must reflect the downgrade, not the startup probe:
  // the header must stop showing the original resource level after fallback.
  assert.equal(state.isolation.effectiveLevel, 'process')
  assert.equal(state.isolation.cgroupEnabled, false)
  assert.equal(state.isolation.cgroupRoot, undefined)
  assert.ok(state.isolation.warnings.some((warning) => warning.includes('auto fallback to process isolation')))
  assert.equal(state.isolationFallbacks.length, 1)
  assert.equal(state.isolationFallbacks[0].fromLevel, 'resource')
  assert.equal(state.isolationFallbacks[0].originalFromLevel, 'resource')
  assert.equal(state.isolationFallbacks[0].toLevel, 'process')
  assert.ok(state.isolationFallbacks[0].warning.includes('auto fallback to process isolation'))
  // The variant keeps the fallback record even after it later fails.
  assert.equal(state.variants.small.status, 'failed')
  assert.equal(state.variants.small.isolationFallback.toLevel, 'process')
})

test('isolation-fallback is a recognized CensorFS event type', () => {
  assert.ok(EVENT_TYPES.includes('isolation-fallback'))
  assert.equal(isCensorFsEvent({ type: 'isolation-fallback', data: { runId: 'r1' } }), true)
})
