import assert from 'node:assert/strict';
import { test } from 'node:test';
import { HostCore, HostError } from '../dist/host/hostCore.js';

test('policy writes reject remote callers and foreign groups before reaching gRPC', async () => {
  const host = new HostCore({}, 'worker');
  for (const [group, address] of [['worker', '192.0.2.1'], ['other', '127.0.0.1'], ['__base__', '::1']]) {
    await assert.rejects(host.applyPolicy({ group, policyYaml: 'rules: []' }, address), HostError);
  }
});

test('host forwards dry-run and expected revision to the actual commit', async () => {
  let applied;
  const host = new HostCore({
    async getPolicy() { return { version: 7 }; },
    async validatePolicy() { return { ok: true }; },
    async applyPolicy(...args) { applied = args; return { version: 7 }; },
  }, 'worker');
  await host.applyPolicy({ group: 'worker', policyYaml: 'rules: []', expectedVersion: 7, dryRun: true }, '::1');
  assert.equal(applied[2], true);
  assert.equal(applied[4], 7);
});

test('a revision change during validation is presented as a conflict', async () => {
  let version = 7;
  const host = new HostCore({
    async getPolicy() { return { version }; },
    async validatePolicy() { version++; return { ok: true }; },
    async applyPolicy(_group, _yaml, _preview, _context, expectedRevision) {
      assert.equal(expectedRevision, 7);
      assert.equal(version, 8);
      throw Object.assign(new Error('stale revision'), { code: 9 });
    },
  }, 'worker');
  await assert.rejects(host.applyPolicy({ group: 'worker', policyYaml: 'rules: []', expectedVersion: 7 }, '127.0.0.1'),
    error => error instanceof HostError && error.kind === 'version_conflict' && error.code === 409);
});
