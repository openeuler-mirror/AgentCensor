import test from 'node:test'
import assert from 'node:assert/strict'
import { losslessJson } from '../src/lossless-json.js'

test('lossless JSON projection removes absent optional exploration fields', () => {
  const projected = losslessJson({
    runId: 'run-1',
    variants: [
      {
        variantId: 'prepared',
        candidateId: 'candidate-1',
        error: undefined,
      },
      {
        variantId: 'failed',
        candidateId: undefined,
        validation: undefined,
        error: 'failed',
      },
    ],
  })

  assert.deepEqual(projected, JSON.parse(JSON.stringify(projected)))
  assert.equal(Object.hasOwn(projected.variants[0], 'error'), false)
  assert.equal(Object.hasOwn(projected.variants[1], 'candidateId'), false)
  assert.equal(Object.hasOwn(projected.variants[1], 'validation'), false)
})
