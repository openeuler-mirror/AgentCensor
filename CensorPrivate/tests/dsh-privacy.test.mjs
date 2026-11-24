import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import { createHash } from 'node:crypto';
import { privacyHandler, recordSessionId, registerPrivacyBridge } from '../plugins/dsh-censorprivacy/privacy-host.mjs';

const owner = { principal: { kind: 'owner' } };
const session = 'test-session_1:main';
const signal = () => new AbortController().signal;

test('privacy bridge scopes exact gateway snapshots to the Harness session and keeps transport errors private', async () => {
  let mode = 'record'; const paths = [];
  const id = recordSessionId(session);
  assert.equal(id, createHash('sha256').update(`censorprivacy-conversation-v1\0deepseek-harness\0${session}`).digest('hex').slice(0, 32));
  const server = createServer((request, response) => {
    paths.push(request.url);
    assert.equal(request.headers.authorization, undefined, 'no credential is needed for local record reads');
    response.setHeader('content-type', 'application/json');
    if (mode === 'missing') { response.writeHead(404).end('{}'); return; }
    if (mode === 'failure') { response.writeHead(503).end('{"error":"private-provider-body"}'); return; }
    response.end(JSON.stringify({ session_id: id, exchanges: [{
      id: 'exchange', session_id: id, agent: mode === 'wrong-agent' ? 'xiaoo' : 'deepseek-harness',
      model: 'model', status: 'completed', created_at: 1,
      messages: [{ role: 'system' }, { role: 'user' }, { role: 'tool' }], response: { secret: 'not-for-this-view' },
      snapshots: ['system', 'user', 'tool'].map((role, i) => ({ path: `/messages/${i}/content`, text: `${role} 原文`,
        analysis: { masked_text: `${role} masked`, results: [], masked_count: 0 } })),
    }] }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    const handler = privacyHandler({ broker: `http://127.0.0.1:${server.address().port}`, mode: 'gateway' });
    const call = () => handler('session', { sessionId: session }, signal(), owner);
    const result = await call();
    assert.equal(result.ok, true);
    assert.equal(result.value.state, 'ready');
    assert.deepEqual(result.value.exchanges[0].snapshots.map(s => s.role), ['system', 'user', 'tool']);
    assert.equal(result.value.exchanges[0].snapshots[1].analysis.masked_text, 'user masked');
    assert.ok(!JSON.stringify(result).includes('not-for-this-view'));
    assert.deepEqual(paths, [`/api/conversations/${id}`]);
    mode = 'missing'; assert.deepEqual((await call()).value, { state: 'empty', exchanges: [] });
    for (mode of ['failure', 'wrong-agent']) {
      assert.equal((await call()).error.message, 'privacy_unavailable');
      assert.ok(!JSON.stringify(await call()).includes('private-provider-body'));
    }
    const before = paths.length;
    for (const payload of [null, [], {}, { sessionId: '../escape' }, { sessionId: 'a/b' }, { sessionId: '' },
      { sessionId: session, url: 'http://example.com' }, { sessionId: 'x'.repeat(129) }]) {
      assert.equal((await handler('session', payload, signal(), owner)).ok, false);
    }
    assert.equal((await handler('sessions', { sessionId: session }, signal(), owner)).ok, false);
    assert.equal((await handler('session', { sessionId: session }, signal(), { principal: { kind: 'agent' } })).ok, false);
    assert.equal(paths.length, before, 'invalid requests must not access the gateway');
    assert.notEqual(recordSessionId('other-session'), id);
    const tls = privacyHandler({ broker: `http://127.0.0.1:${server.address().port}`, mode: 'tls' });
    assert.equal((await tls('session', { sessionId: session }, signal(), owner)).value.state, 'tls');
    assert.equal(paths.length, before, 'TLS mode must not fabricate gateway records');
    const abort = new AbortController(); abort.abort();
    assert.equal((await handler('session', { sessionId: session }, abort.signal, owner)).ok, false);
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
  for (const broker of ['https://127.0.0.1:8775', 'http://example.com', 'http://127.0.0.1:8775/redirect', 'http://user@127.0.0.1:8775']) {
    assert.throws(() => privacyHandler({ broker }), /local_broker_required/);
  }
});

test('web bridge uses Connection lifetime and client registers the native tab after trajectory', async () => {
  const registrations = [];
  let dispose, disposed = false;
  registerPrivacyBridge({ inject(services, fn) {
    assert.deepEqual(services, ['connection', 'webServer']);
    fn({ effect(factory) { dispose = factory(); }, connection: { fetch: { register(route) {
      registrations.push(route); return () => { disposed = true; };
    } } } });
  } }, { mode: 'tls', broker: 'http://127.0.0.1:8775' });
  const route = registrations[0];
  assert.equal(route.path, '/api/censorprivacy/session');
  assert.deepEqual(route.methods, ['POST']);
  assert.equal((await route.fetch(new Request('http://localhost' + route.path, { method: 'POST', body: '{}' }))).status, 400);
  const response = await route.fetch(new Request('http://localhost' + route.path, { method: 'POST',
    body: JSON.stringify({ type: 'client-request', rpcId: 'correlation-id', method: 'censorprivacy/session', payload: { sessionId: session } }),
  }));
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.deepEqual(await response.json(), { type: 'server-response', rpcId: 'correlation-id', result: { ok: true, value: { state: 'tls', exchanges: [] } } });
  await dispose(); assert.equal(disposed, true, 'unloading the plugin must remove its exact route');
  let client;
  runInNewContext(await readFile(new URL('../plugins/dsh-censorprivacy/lib/client.js', import.meta.url), 'utf8'), {
    window: { __ModuleLoader__: { load({ id, factory }) {
      assert.equal(id, 'dsh-censorprivacy'); client = factory(name => { assert.equal(name, 'react'); return {}; });
    } } },
  });
  let tab;
  const connection = {};
  client.apply({ connection, slots: {
    inject(slot, fn) { assert.equal(slot, 'conversation.view'); fn(); },
    register(options, component) { tab = options; assert.equal(typeof component, 'function'); },
  } });
  assert.equal(tab.id, 'privacy'); assert.equal(tab.label, '隐私'); assert.equal(tab.order, 15);
  assert.equal(tab.inject('current-session').connection, connection);
  assert.equal(tab.inject('current-session').privacySessionId, 'current-session');
});
