import test from 'node:test'
import assert from 'node:assert/strict'
import { derivedRequestId } from '../src/censorfs-cli.js'

test('composite request ids are stable per run, variant, and phase', () => {
  const first = derivedRequestId('run-1', 'minimal', 'prepare')
  assert.equal(first, derivedRequestId('run-1', 'minimal', 'prepare'))
  assert.notEqual(first, derivedRequestId('run-1', 'defensive', 'prepare'))
  assert.notEqual(first, derivedRequestId('run-1', 'minimal', 'publish'))
  assert.match(first, /^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u)
})
