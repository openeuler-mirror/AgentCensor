import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createBootstrap } from '../dist/bootstrap/index.js';
import { AttachSelfClient } from '../dist/runtime/index.js';

test('nonblocking degraded bootstrap is immediately ready', async () => {
  const core = createBootstrap({ policyGroup: 'worker' });
  core.stateMachine.toDegraded('daemon unavailable');
  try { await core.waitForReady(20); }
  finally { core.dispose(); }
});

test('timed-out readiness waits release their state listeners', async () => {
  const core = createBootstrap({ policyGroup: 'worker', blockOnFailure: true });
  try {
    await assert.rejects(core.waitForReady(1), /等待超时/);
    assert.equal(core.stateMachine.listeners.size, 0);
  } finally { core.dispose(); }
});

test('disposing bootstrap cancels pending readiness waits', async () => {
  const core = createBootstrap({ policyGroup: 'worker', blockOnFailure: true });
  const waiting = core.waitForReady(1000);
  core.dispose();
  await assert.rejects(waiting, /dispose/);
  assert.equal(core.stateMachine.listeners.size, 0);
});

test('failed attachment retries back off, reset after success and stop on disposal', async () => {
  const original = {
    setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout,
    attachSelf: AttachSelfClient.prototype.attachSelf,
    startHeartbeat: AttachSelfClient.prototype.startHeartbeat,
  };
  const pending = new Map();
  let nextId = 0;
  let fail = true;
  globalThis.setTimeout = (callback, delay) => {
    const id = ++nextId;
    pending.set(id, { callback, delay });
    return id;
  };
  globalThis.clearTimeout = (id) => pending.delete(id);
  AttachSelfClient.prototype.attachSelf = async () => {
    if (fail) throw new Error('daemon unavailable');
    return { domainId: 1, domain: 'worker', group: 'worker', version: 1,
      daemonBootId: 'test-boot', hooksHealthy: true };
  };
  AttachSelfClient.prototype.startHeartbeat = () => {};
  const core = createBootstrap({ policyGroup: 'worker', initialRetryMs: 10, maxRetryMs: 40 });
  const tick = async (expectedDelay) => {
    assert.equal(pending.size, 1);
    const [id, timer] = [...pending][0];
    assert.equal(timer.delay, expectedDelay);
    pending.delete(id);
    timer.callback();
    await new Promise(setImmediate);
  };
  try {
    await assert.rejects(core.start(), /daemon unavailable/);
    await tick(10);
    await tick(20);
    await tick(40);
    fail = false;
    await tick(40);
    assert.equal(core.state.kind, 'protected');
    assert.equal(pending.size, 0);
    fail = true;
    await core.reattach();
    assert.equal([...pending.values()][0].delay, 10);
    core.dispose();
    assert.equal(pending.size, 0);
  } finally {
    core.dispose();
    globalThis.setTimeout = original.setTimeout;
    globalThis.clearTimeout = original.clearTimeout;
    AttachSelfClient.prototype.attachSelf = original.attachSelf;
    AttachSelfClient.prototype.startHeartbeat = original.startHeartbeat;
  }
});
