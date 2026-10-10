import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';
import { build } from 'esbuild';
import { AuditStream } from '../dist/host/auditStream.js';

const compiled = await build({ entryPoints: [new URL('../src/client/controllers.ts', import.meta.url).pathname], bundle: true, write: false, format: 'esm', platform: 'node' });
const { AuditController } = await import(`data:text/javascript;base64,${Buffer.from(compiled.outputFiles[0].text).toString('base64')}`);
const tick = () => new Promise(resolve => setImmediate(resolve));
const event = (sequence, daemonBootId = 'first') => ({
  ts: '1', kind: 2, op: 1, pid: 123, tgid: 123, allowed: false,
  policyVersion: 1, ruleVersion: 1, comm: 'probe', detail: '/bin/id', args: [],
  domainId: 1, domain: 'test', sequence, daemonBootId, droppedBefore: 0,
});
const snapshot = (sequence = 0, boot = 'first') => ({
  entries: sequence ? [{ type: 'event', event: event(sequence, boot) }] : [],
  lastSequence: sequence,
  status: { state: 'live', daemonBootId: boot, lastSequence: sequence, droppedCount: 0, evictedCount: 0, cachedCount: sequence ? 1 : 0, lastError: '' },
});

test('audit tab can leave and re-enter without duplicate or abandoned requests', async () => {
  let snapshots = 0;
  const waits = [];
  const controller = new AuditController({ isLoopback: true, rpc: {
    async call(_channel, endpoint, _payload, signal) {
      if (endpoint === 'audit/snapshot') return { ok: true, value: snapshot(++snapshots) };
      waits.push(signal);
      return new Promise((_resolve, reject) => signal?.addEventListener('abort', () => reject(signal.reason), { once: true }));
    },
  }});
  try {
    controller.start();
    controller.start();
    await tick();
    assert.equal(snapshots, 1, 'one loop per mounted tab');
    controller.dispose();
    assert.equal(waits[0]?.aborted, true, 'leaving cancels the pending HTTP wait');
    controller.start();
    await tick();
    assert.equal(snapshots, 2, 'returning to the tab resumes polling');
    assert.equal(waits.length, 2);
    assert.equal(controller.getSnapshot().items.filter(item => item.type === 'row').length, 2);
  } finally { controller.dispose(); }
});

test('browser audit cursor follows daemon boot changes and accepts reused sequence numbers', async () => {
  const requests = [];
  const controller = new AuditController({ isLoopback: true, rpc: {
    async call(_channel, endpoint, payload, signal) {
      if (endpoint === 'audit/snapshot') return { ok: true, value: snapshot(20) };
      requests.push(payload);
      if (requests.length === 1) return { ok: true, value: snapshot(1, 'second') };
      return new Promise((_resolve, reject) => signal?.addEventListener('abort', () => reject(signal.reason), { once: true }));
    },
  }});
  try {
    controller.start();
    await tick();
    assert.equal(requests[0].afterBootId, 'first');
    assert.equal(requests[1].afterBootId, 'second');
    assert.equal(requests[1].afterSequence, 1);
    assert.equal(controller.eventAt(1)?.daemonBootId, 'second');
  } finally { controller.dispose(); }
});

test('host wakes old-boot waiters and resets sequence when daemon restarts', async () => {
  const stream = new EventEmitter();
  stream.cancel = () => {};
  const audit = new AuditStream({ subscribeEvents: async () => stream }, { domainIds: [1] });
  audit.start();
  await tick();
  try {
    stream.emit('data', event(20));
    const pending = audit.wait(20, 200, 1000, undefined, undefined, 'first');
    stream.emit('data', event(1, 'second'));
    const result = await pending;
    assert.equal(result.lastSequence, 1);
    assert.equal(result.status.daemonBootId, 'second');
    assert(result.entries.some(entry => entry.type === 'gap' && entry.reason === 'daemon_restart'));
    assert(result.entries.some(entry => entry.type === 'event' && entry.event.sequence === 1));
    assert.equal(audit.snapshot(20, 200, undefined, 'first').entries.length, result.entries.length);
  } finally { audit.dispose(); }
});

test('host releases a long-poll waiter immediately when the browser disconnects', async () => {
  const audit = new AuditStream({ subscribeEvents: async () => { throw new Error('unused'); } }, { domainIds: [1] });
  const abort = new AbortController();
  try {
    const pending = audit.wait(0, 200, 25000, undefined, abort.signal);
    abort.abort();
    const result = await Promise.race([pending, new Promise(resolve => setTimeout(() => resolve(null), 50))]);
    assert.notEqual(result, null, 'disconnect must not retain the waiter for 25 seconds');
  } finally { audit.dispose(); }
});
