import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';
import { build } from 'esbuild';
import { AuditStream } from '../dist/host/auditStream.js';
import { AuditBuffer } from '../dist/ui/auditTab.js';
import { statusBar } from '../dist/ui/auditView.js';

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

function fakeStream() {
  const stream = new EventEmitter();
  stream.cancel = () => stream.emit('error', new Error('cancelled'));
  return stream;
}

test('filtered global sequence gaps are normal; explicit loss is counted once', async () => {
  const stream = fakeStream();
  const audit = new AuditStream({ subscribeEvents: async () => stream }, { domainIds: [1] });
  audit.start();
  await tick();
  try {
    stream.emit('data', event(10));
    stream.emit('data', { ...event(100), droppedBefore: 3 });
    assert.equal(audit.status().droppedCount, 3);
    assert.deepEqual(audit.snapshot(0, 200).entries.filter(e => e.type === 'gap').map(e => e.reason), ['dropped']);
    const page = audit.snapshot(10, 1);
    assert.equal(page.entries.length, 2, 'loss marker and real event share an atomic page');
    assert.equal(page.entries[1].event.sequence, 100);
  } finally { audit.dispose(); }
});

test('disconnect never advances the browser past the first recovered event', async () => {
  const streams = [fakeStream(), fakeStream()];
  let count = 0;
  const audit = new AuditStream({ subscribeEvents: async () => streams[count++] }, { domainIds: [1] }, { reconnectInitialMs: 1 });
  audit.start();
  await tick();
  try {
    streams[0].emit('data', event(10));
    const pending = audit.wait(10, 1, 1000);
    streams[0].emit('error', new Error('connection lost'));
    const offline = await pending;
    assert.equal(offline.status.state, 'reconnecting');
    assert.equal(offline.entries.length, 0);
    assert.equal(offline.lastSequence, 10);
    await new Promise(resolve => setTimeout(resolve, 10));
    streams[1].emit('data', event(11));
    const recovered = audit.snapshot(10, 1);
    assert.equal(recovered.entries[0].reason, 'stream_reconnect');
    assert.equal(recovered.entries[1].event.sequence, 11);
    assert.equal(audit.status().droppedCount, 0, 'disconnect loss count is unknown');
  } finally { audit.dispose(); }
});

test('domain changes cancel a stale pending subscription; equal updates keep the stream', async () => {
  let finish;
  const stale = fakeStream();
  let cancelled = 0;
  stale.cancel = () => { cancelled++; };
  const current = fakeStream();
  const requests = [];
  const audit = new AuditStream({ subscribeEvents: async request => {
    requests.push(request);
    return requests.length === 1 ? new Promise(resolve => { finish = resolve; }) : current;
  } }, { domainIds: [0], observerTgid: 123 });
  audit.start();
  audit.setDomainIds([7]);
  finish(stale);
  await tick();
  try {
    assert.equal(cancelled, 1);
    assert.deepEqual(requests.map(r => r.domainIds), [[0], [7]]);
    assert.equal(requests[1].observerTgid, 123);
    audit.setDomainIds([7]);
    await tick();
    assert.equal(requests.length, 2);
    current.emit('data', event(20));
    assert.equal(audit.status().lastSequence, 20);
  } finally { audit.dispose(); }
});

test('cache removal counters belong to separate caches and reset on clear', () => {
  const buffer = new AuditBuffer(1);
  buffer.ingest([ { type: 'event', event: event(1) }, { type: 'event', event: event(2) } ]);
  assert.equal(buffer.evictedCount(), 1);
  const bar = statusBar({ ...snapshot().status, evictedCount: 1 }, buffer.evictedCount(), 1);
  assert.equal(bar.hostEvictedCount, 1);
  assert.equal(bar.localEvictedCount, 1);
  buffer.clear();
  assert.equal(buffer.evictedCount(), 0);
});
