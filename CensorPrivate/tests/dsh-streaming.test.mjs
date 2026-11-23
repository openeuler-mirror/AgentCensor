import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';
import { chatSSE } from '../plugins/dsh-censorprivacy/openai-chat.mjs';
import { PrivacyAdapter } from '../plugins/dsh-censorprivacy/index.mjs';

const frame = (delta, finish_reason = null) => ({ choices: [{ index: 0, delta, finish_reason }] });
const wire = data => 'data: ' + JSON.stringify(data) + '\n\n';
const end = wire(frame({}, 'stop')) + 'data: [DONE]\n\n';
async function* bytes(text, step = 1) {
  const raw = Buffer.from(text);
  for (let i = 0; i < raw.length; i += step) yield raw.subarray(i, i + step);
}
async function collect(source) { const values = []; for await (const value of source) values.push(value); return values; }

test('protocol decoder handles split UTF-8, CRLF, comments, multiline SSE and trailing usage', async () => {
  const first = 'event: message\r\n: heartbeat\r\ndata: {"choices":\r\ndata: [{"index":0,"delta":{"role":"assistant","content":"你好😀"},"finish_reason":null}]}\r\n\r\n';
  const usage = wire({ choices: [], usage: { prompt_tokens: 7, completion_tokens: 2 } });
  const events = await collect(chatSSE(bytes(first + wire(frame({}, 'stop')) + usage + 'data: [DONE]\n\n')));
  assert.deepEqual(events, [{ type: 'text', text: '你好😀' }, { type: 'usage', inputTokens: 7, outputTokens: 2 }, { type: 'finish', reason: 'stop' }]);
});

test('fragmented parallel tool calls complete only after DONE, sorted by protocol index', async () => {
  const fragments = [frame({ tool_calls: [
    { index: 1, id: 'second', type: 'function', function: { name: 'private_', arguments: '{"operation":' } },
    { index: 0, id: 'first', type: 'function', function: { name: 'private_compute', arguments: '{}' } },
  ] }), frame({ tool_calls: [{ index: 1, function: { name: 'compute', arguments: '"sum","refs":[]}' } }] }), frame({}, 'tool_calls')];
  const text = fragments.map(wire).join('');
  await assert.rejects(collect(chatSSE(bytes(text))), /response_invalid/);
  const events = await collect(chatSSE(bytes(text + 'data: [DONE]\n\n', 7)));
  assert.deepEqual(events.map(event => event.type), ['tool-call', 'tool-call', 'finish']);
  assert.deepEqual(events.slice(0, 2).map(event => event.id), ['first', 'second']);
  assert.equal(JSON.parse(events[1].arguments).operation, 'sum');
});

test('invalid frames never release their text or incomplete tool calls', async () => {
  for (const bad of [frame({ content: 'hidden', reasoning_content: 'opaque' }), frame({ content: 'hidden', refusal: 'no' }),
    frame({ content: 'hidden', extra: 'unknown' }), { ...frame({ content: 'hidden' }), error: { message: 'private' } },
    { choices: [{ index: 1, delta: { content: 'hidden' } }] }, frame({ content: 'hidden', tool_calls: [{ index: -1 }] })]) {
    const emitted = [];
    await assert.rejects(async () => { for await (const event of chatSSE(bytes(wire(bad) + end))) emitted.push(event); }, /response_invalid/);
    assert.deepEqual(emitted, []);
  }
  for (const bad of [wire(frame({}, 'stop')), 'data: [DONE]\n\n',
    wire(frame({ tool_calls: [{ index: 0, id: 'bad', type: 'function', function: { name: 'tool', arguments: '{' } }] }, 'tool_calls')) + 'data: [DONE]\n\n',
    wire(frame({}, 'stop')) + wire(frame({ content: 'after finish' })) + 'data: [DONE]\n\n',
    'event: error\ndata: {"error":"private"}\n\n']) {
    await assert.rejects(collect(chatSSE(bytes(bad))), /response_invalid/);
  }
});

test('adapter yields text before response completion and assembles DSH blocks without duplication', async () => {
  let finishResponse, requestBody;
  const ending = new Promise(ok => { finishResponse = ok; });
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    requestBody = JSON.parse(Buffer.concat(chunks));
    response.writeHead(200, { 'content-type': 'text/event-stream; charset=utf-8' });
    response.write(wire(frame({ role: 'assistant', content: '第一段😀' })));
    await ending;
    response.end(wire(frame({ content: '第二段' })) + end);
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    const adapter = new PrivacyAdapter({ mode: 'gateway', baseURL: `http://127.0.0.1:${server.address().port}`, timeoutMs: 3000 });
    const iterator = adapter.stream({ model: 'model', messages: [] });
    assert.equal((await iterator.next()).value.type, 'block-start');
    const first = await iterator.next();
    assert.deepEqual(first.value, { type: 'text-delta', index: 0, text: '第一段😀' });
    assert.equal(requestBody.stream, true);
    finishResponse();
    const rest = await collect(iterator);
    assert.equal(rest.find(event => event.type === 'block-end').block.text, '第一段😀第二段');
    assert.equal(rest.at(-1).type, 'finish');
  } finally { finishResponse(); server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});

test('stream deadlines, cancellation and consumer exit close the transport', { timeout: 10000 }, async () => {
  let closed = 0;
  const server = createServer((request, response) => {
    request.resume();
    response.on('close', () => closed++);
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    response.write(wire(frame({ content: 'approved prefix' })));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  const baseURL = `http://127.0.0.1:${server.address().port}`;
  const options = { model: 'model', messages: [] };
  try {
    const timed = new PrivacyAdapter({ mode: 'gateway', baseURL, timeoutMs: 100 });
    await assert.rejects(collect(timed.stream(options)), /PRIVACY_TRANSPORT_FAILED/);
    const controller = new AbortController();
    const adapter = new PrivacyAdapter({ mode: 'gateway', baseURL, timeoutMs: 3000 });
    await assert.rejects(async () => {
      for await (const event of adapter.stream({ ...options, signal: controller.signal })) {
        if (event.type === 'text-delta') controller.abort();
      }
    }, /PRIVACY_TRANSPORT_FAILED/);
    for await (const event of adapter.stream(options)) if (event.type === 'text-delta') break;
    for (let i = 0; closed < 3 && i < 100; i++) await delay(10);
    assert.equal(closed, 3);
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});

test('truncated or erroneous HTTP streams never emit finish or execute a partial tool', async () => {
  let suffix = '';
  const server = createServer((request, response) => {
    request.resume(); response.setHeader('content-type', 'text/event-stream');
    response.end(wire(frame({ content: 'approved prefix' })) + suffix);
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    for (suffix of ['', wire({ error: { message: 'private upstream error' } }), wire(frame({}, 'stop'))]) {
      const emitted = [];
      const adapter = new PrivacyAdapter({ mode: 'gateway', baseURL: `http://127.0.0.1:${server.address().port}`, timeoutMs: 1000 });
      await assert.rejects(async () => { for await (const event of adapter.stream({ model: 'model', messages: [] })) emitted.push(event); }, /UNSUPPORTED_RESPONSE/);
      assert.equal(emitted.filter(event => event.type === 'text-delta').map(event => event.text).join(''), 'approved prefix');
      assert.ok(emitted.every(event => !['finish', 'tool-call-delta'].includes(event.type)));
    }
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});

test('oversized, compressed and unexpected response types fail without exposing error bodies', async () => {
  let mode;
  const server = createServer((request, response) => {
    request.resume();
    if (mode === 'http-error') {
      response.writeHead(400, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: { code: 'detection_failed', message: 'do-not-print-provider-body' } })); return;
    }
    response.setHeader('content-type', mode === 'html' ? 'text/html' : 'text/event-stream');
    if (mode === 'compressed') response.setHeader('content-encoding', 'gzip');
    response.end(mode === 'oversized' ? ':' + 'x'.repeat(1024 * 1024) : wire(frame({ content: 'do-not-emit' })) + end);
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    for (mode of ['http-error', 'html', 'compressed', 'oversized']) {
      const adapter = new PrivacyAdapter({ mode: 'gateway', baseURL: `http://127.0.0.1:${server.address().port}`, timeoutMs: 1000 });
      const emitted = [];
      const expected = mode === 'http-error' ? 'UPSTREAM_HTTP_400_DETECTION_FAILED'
        : mode === 'html' ? 'UNSUPPORTED_RESPONSE' : 'PRIVACY_TRANSPORT_FAILED';
      await assert.rejects(async () => { for await (const event of adapter.stream({ model: 'model', messages: [] })) emitted.push(event); },
        error => error.message === `CensorPrivacy: ${expected}`);
      assert.deepEqual(emitted, []);
    }
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});
