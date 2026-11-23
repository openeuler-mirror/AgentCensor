import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { serialize, privateResult, PrivacyAdapter, apply } from '../plugins/dsh-censorprivacy/index.mjs';
import { requestJSON, tlsJSON } from '../plugins/dsh-censorprivacy/transport.mjs';

const reference = '<CP_MONEY_0123wxyz>';
test('DSH 0.2.2 tool messages preserve call identity, reference results and recoverable errors', () => {
  const messages = [
    { role: 'assistant', content: [{ type: 'tool-call', id: 'compute-one', name: 'private_compute', arguments: '{}' }] },
    { role: 'tool', toolCallId: 'compute-one', content: [{ type: 'text', text: JSON.stringify({ result: reference }) }] },
    { role: 'tool', toolCallId: 'compute-two', isError: true, content: [{ type: 'text', text: 'DATE_DIFF_REQUIRES_TWO_DATE_REFERENCES' }] },
  ];
  const wire = serialize({ model: 'model', messages }, {});
  assert.deepEqual(wire.messages.slice(1), [
    { role: 'tool', tool_call_id: 'compute-one', content: JSON.stringify({ result: reference }) },
    { role: 'tool', tool_call_id: 'compute-two', content: 'DATE_DIFF_REQUIRES_TWO_DATE_REFERENCES' },
  ]);
  for (const tool of [
    { ...messages[1], toolCallId: undefined }, { ...messages[1], toolCallId: '' },
    { ...messages[1], isError: 'yes' }, { ...messages[1], content: [{ type: 'image' }] },
  ]) assert.throws(() => serialize({ model: 'model', messages: [tool] }, {}), /UNSUPPORTED_CONTENT/);
});
test('Harness automatically attaches real session metadata locally, with an explicit fallback for older versions', async () => {
  const calls = [];
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    calls.push({ headers: request.headers, body: JSON.parse(Buffer.concat(chunks)) });
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ choices: [{ message: { role: 'assistant', content: 'OK' }, finish_reason: 'stop' }] }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    const config = { mode: 'gateway', baseURL: `http://127.0.0.1:${server.address().port}` };
    const adapter = new PrivacyAdapter(config);
    const options = { model: 'model', messages: [{ role: 'user', content: [{ type: 'text', text: 'Hello' }] }] };
    for (const sessionId of ['session-one', 'session-one', 'session-two']) {
      for await (const _ of adapter.stream({ ...options, sessionId })) { /* consume */ }
    }
    for await (const _ of adapter.stream({ ...options, sessionId: 'session-one', purpose: 'session-title' })) { /* consume */ }
    for await (const _ of adapter.stream(options)) { /* consume */ }
    const fallback = new PrivacyAdapter({ ...config, conversationSession: 'configured-session' });
    for await (const _ of fallback.stream(options)) { /* consume */ }
    for await (const _ of fallback.stream({ ...options, sessionId: 'real-session' })) { /* consume */ }
    assert.deepEqual(calls.map(call => call.headers['x-censorprivacy-session']),
      ['session-one', 'session-one', 'session-two', undefined, undefined, 'configured-session', 'real-session']);
    for (const call of calls) {
      assert.equal(call.headers['x-censorprivacy-agent'], call.headers['x-censorprivacy-session'] ? 'deepseek-harness' : undefined);
      assert.equal(call.body.sessionId, undefined);
      assert.equal(call.body.agent, undefined);
    }
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});
test('private-only requests hide agent-scoped tools and preserve supported compute schemas', () => {
  const options = { model: 'model', messages: [], tools: [
    { name: 'private_compute', parameters: { type: 'object' } },
    { name: 'subagent', parameters: { type: 'object' } },
    { name: 'bash', parameters: { type: 'object' } },
  ] };
  assert.deepEqual(serialize(options, { privateToolsOnly: true }).tools.map(tool => tool.function.name), ['private_compute']);
  assert.deepEqual(serialize(options, {}).tools.map(tool => tool.function.name), ['private_compute']);
  assert.equal(serialize(options, { privateToolsOnly: false }).tools.length, 3);
  assert.equal(serialize({ ...options, tools: [options.tools[1]] }, { privateToolsOnly: true }).tools, undefined);
});
test('invalid date arithmetic is a recoverable local tool error; valid arithmetic still calls the broker', async () => {
  const date = reference.replace('MONEY', 'DATE');
  let tool, requests = 0;
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const args = JSON.parse(Buffer.concat(chunks)); requests++;
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ operation: args.operation, results: { sum: reference }, entity_type: 'MONEY', unit: 'CNY', result: reference }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  const original = process.env.CENSORPRIVACY_TOKEN; process.env.CENSORPRIVACY_TOKEN = 'synthetic-test-token';
  try {
    const baseURL = `http://127.0.0.1:${server.address().port}`;
    apply({ llm: { registerAdapter() {} }, tools: { register(value) { tool = value; }, guard() {} }, on() {} }, { mode: 'gateway', baseURL, broker: baseURL, model: 'model' });
    await assert.rejects(tool.execute({ operation: 'date_diff', refs: [date], options: { reference: 'today' } }, {}), /DATE_DIFF_REQUIRES_TWO_DATE_REFERENCES/);
    for (const options of [{ reference: 'today' }, { unit: 'weeks' }, null]) {
      await assert.rejects(tool.execute({ operation: 'date_diff', refs: [date, date], options }, {}), /COMPUTE_OPTIONS_INVALID/);
    }
    await assert.rejects(tool.execute({ operation: 'sum', refs: [reference], options: { reference: 'today' } }, {}), /COMPUTE_OPTIONS_INVALID/);
    await assert.rejects(tool.execute({ operation: 'aggregate', refs: [reference], options: { operations: ['date_diff'] } }, {}), /COMPUTE_OPTIONS_INVALID/);
    assert.equal(requests, 0, 'invalid arguments must not reach the broker');
    assert.equal((await tool.execute({ operation: 'sum', refs: [reference] }, {})).result, reference);
    assert.equal(requests, 1, 'a tool error must not prevent the next valid call');
  } finally {
    if (original === undefined) delete process.env.CENSORPRIVACY_TOKEN; else process.env.CENSORPRIVACY_TOKEN = original;
    server.closeAllConnections(); await new Promise(ok => server.close(ok));
  }
});
test('private tool catalog drives advertisement and execution independently of the native tools toggle', async () => {
  const catalog = [{ name: 'private_compute', kind: 'builtin' },
    { name: 'private_sort', kind: 'program', description: 'Sort the private values.', output: 'order' }];
  const other = reference.replace('0123', 'abcd');
  let raw = false; const calls = [];
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    calls.push({ path: request.url, args: JSON.parse(Buffer.concat(chunks)) });
    response.end(JSON.stringify({ refs: raw ? ['raw value', reference] : [other, reference] }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  const original = process.env.CENSORPRIVACY_TOKEN; process.env.CENSORPRIVACY_TOKEN = 'synthetic-token';
  try {
    const baseURL = `http://127.0.0.1:${server.address().port}`;
    for (const enabled of [false, true]) {
      let guard, allowed, adapter; const registered = new Map();
      apply({
        llm: { registerAdapter(_ids, value) { adapter = value; } },
        tools: { register(tool) { registered.set(tool.name, tool); }, guard(fn) { guard = fn; } },
        on(_event, fn) { fn({ agent: { ctx: { tools: { restrict(spec) { allowed = spec.allow; } } } } }); },
      }, { mode: 'gateway', baseURL, broker: baseURL, model: 'test', privateToolsOnly: !enabled, privateTools: catalog });
      const options = { model: 'test', messages: [], tools: [...registered.values(), { name: 'bash' }] };
      assert.deepEqual(serialize(options, adapter.config).tools.map(tool => tool.function.name),
        enabled ? ['private_compute', 'private_sort', 'bash'] : ['private_compute', 'private_sort']);
      const sort = registered.get('private_sort');
      assert.equal(registered.get('private_compute').parameters.properties.refs.items.pattern,
        '^<CP_[A-Z][A-Z0-9_]*_[a-z0-9]{8}>$');
      if (enabled) { assert.equal(guard, undefined); continue; }
      assert.deepEqual(allowed, ['private_compute', 'private_sort']);
      assert.equal(guard({ name: 'private_sort' }), undefined);
      assert.match(guard({ name: 'bash' }), /PRIVATE_TOOLS_ONLY/);
      await assert.rejects(sort.execute({ refs: [reference], command: 'never' }, {}), /REFERENCES_REQUIRED/);
      const refs = [reference, other];
      assert.deepEqual((await sort.execute({ refs }, {})).refs, [other, reference]);
      assert.deepEqual(calls, [{ path: '/v1/private_tools/private_sort', args: { refs } }]);
      raw = true;
      await assert.rejects(sort.execute({ refs }, {}), /PRIVATE_RESULT_REQUIRED/);
    }
  } finally {
    if (original === undefined) delete process.env.CENSORPRIVACY_TOKEN; else process.env.CENSORPRIVACY_TOKEN = original;
    server.closeAllConnections(); await new Promise(ok => server.close(ok));
  }
});
test('DSH rejects unsupported content and never returns raw compute results or extra fields', () => {
  const options = { model: 'model', messages: [{ role: 'user', content: [{ type: 'text', text: 'Hello' }] }] };
  assert.equal(serialize(options, { maxTokens: 512 }).stream, true);
  assert.equal(serialize(options, { mode: 'tls', maxTokens: 512 }).stream, false);
  assert.equal(serialize(options, { maxTokens: 512 }).thinking, undefined);
  for (const block of [{ type: 'image' }, { type: 'reasoning', text: 'private' }, { type: 'text', text: 'Hello', secret: 'hidden' }]) {
    assert.throws(() => serialize({ ...options, messages: [{ role: 'user', content: [block] }] }, {}), /UNSUPPORTED_CONTENT/);
  }
  assert.throws(() => serialize({ ...options, reasoningEffort: 'high' }, {}), /UNSUPPORTED_OPTION/);
  const result = { operation: 'sum', entity_type: 'MONEY', unit: 'CNY', results: { sum: reference }, result: reference };
  assert.equal(privateResult(result, 'sum'), result);
  for (const invalid of [{ ...result, raw: '300' }, { ...result, result: '300' }, { ...result, results: { sum: 300 } }, { ...result, unit: 'sensitive-text' }]) {
    assert.throws(() => privateResult(invalid, 'sum'), /PRIVATE_RESULT_REQUIRED/);
  }
});
test('DeepSeek calls disable default thinking and still reject unexpected reasoning without partial text', async () => {
  let ignoreThinking = false;
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks));
    requests.push(body);
    response.setHeader('Content-Type', 'application/json');
    const reasoning = ignoreThinking || body.thinking?.type !== 'disabled';
    response.end(JSON.stringify({ choices: [{ message: { role: 'assistant', content: 'OK',
      ...(reasoning ? { reasoning_content: 'synthetic default reasoning' } : {}) }, finish_reason: 'stop' }] }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    const adapter = new PrivacyAdapter({ mode: 'gateway', baseURL: `http://127.0.0.1:${server.address().port}`, timeoutMs: 1000 });
    const options = { model: 'deepseek-flash', messages: [{ role: 'user', content: [{ type: 'text', text: 'Hello' }] }] };
    const chunks = [];
    for await (const chunk of adapter.stream(options)) chunks.push(chunk);
    assert.deepEqual(requests[0].thinking, { type: 'disabled' });
    assert.ok(chunks.some(chunk => chunk.type === 'text-delta' && chunk.text === 'OK'));
    ignoreThinking = true;
    const rejected = [];
    await assert.rejects(async () => { for await (const chunk of adapter.stream(options)) rejected.push(chunk); }, /UNSUPPORTED_RESPONSE/);
    assert.deepEqual(rejected, []);
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});
test('DeepSeek nonstream tool-call indexes are accepted as metadata and malformed indexes are rejected', async () => {
  let index = 0;
  let extra = {};
  const server = createServer((request, response) => {
    request.resume();
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ message: { role: 'assistant', content: '', tool_calls: [{
      id: 'call_test', type: 'function', index, function: { name: 'private_compute', arguments: '{"operation":"sum","refs":[]}' }, ...extra,
    }] }, finish_reason: 'tool_calls' }] }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    const adapter = new PrivacyAdapter({ mode: 'gateway', baseURL: `http://127.0.0.1:${server.address().port}`, timeoutMs: 1000 });
    const options = { model: 'deepseek-flash', messages: [] };
    const chunks = [];
    for await (const chunk of adapter.stream(options)) chunks.push(chunk);
    assert.ok(chunks.some(chunk => chunk.type === 'block-end' && chunk.block.name === 'private_compute'));
    for (const invalid of [-1, 0.5, '0', null]) {
      index = invalid;
      const rejected = [];
      await assert.rejects(async () => { for await (const chunk of adapter.stream(options)) rejected.push(chunk); }, /UNSUPPORTED_RESPONSE/);
      assert.deepEqual(rejected, []);
    }
    index = 0; extra = { raw: 'unexpected field' };
    await assert.rejects(async () => { for await (const chunk of adapter.stream(options)) assert.fail('Unexpected response emitted'); }, /UNSUPPORTED_RESPONSE/);
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});
test('invalid provider responses emit no partial DSH text; HTTP deadlines cover the whole response', { timeout: 10000 }, async () => {
  const server = createServer((request, response) => {
    response.setHeader('Content-Type', 'application/json');
    if (request.url === '/slow') {
      response.write(' '); const interval = setInterval(() => response.write(' '), 10);
      response.on('close', () => clearInterval(interval)); return;
    }
    response.end(JSON.stringify({ choices: [{ message: { role: 'assistant', content: 'must-not-be-emitted', tool_calls: [{ id: 'bad' }] }, finish_reason: 'stop' }] }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  const baseURL = `http://127.0.0.1:${server.address().port}`;
  try {
    const adapter = new PrivacyAdapter({ mode: 'gateway', baseURL, model: 'model', timeoutMs: 1000 });
    const chunks = [];
    await assert.rejects(async () => { for await (const chunk of adapter.stream({ model: 'model', messages: [] })) chunks.push(chunk); }, /UNSUPPORTED_RESPONSE/);
    assert.deepEqual(chunks, []);
    await assert.rejects(requestJSON(`${baseURL}/slow`, {}, { timeoutMs: 80 }));
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});
test('gateway diagnostics expose fixed reason codes without echoing arbitrary error data', async () => {
  const server = createServer((request, response) => {
    request.resume(); response.statusCode = 400; response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ error: { code: request.url === '/fixed' ? 'sensitive_structure' : 'private-user-content', message: 'private-user-content' } }));
  });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  try {
    const base = `http://127.0.0.1:${server.address().port}`;
    await assert.rejects(requestJSON(base + '/fixed', {}), { message: 'upstream_http_400_sensitive_structure' });
    await assert.rejects(requestJSON(base + '/unknown', {}), { message: 'upstream_http_400' });
  } finally { server.closeAllConnections(); await new Promise(ok => server.close(ok)); }
});
test('TLS cancellation waits for the client process to close', { timeout: 10000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-plugin-test-')); const executable = join(directory, 'client'); const pidFile = join(directory, 'pid');
  await writeFile(executable, `#!/usr/bin/env node\nimport('node:fs').then(fs => fs.writeFileSync(${JSON.stringify(pidFile)}, String(process.pid)));\nsetInterval(() => {}, 1000);\n`, { mode: 0o700 });
  const controller = new AbortController();
  const pending = tlsJSON({ baseURL: 'https://127.0.0.1', executable: executable, broker: 'http://127.0.0.1:1', tokenFile: '/unused', runStateFile: '/unused', nativeLibrary: '/unused', workspace: directory }, {}, controller.signal);
  try {
    let pid;
    for (let i = 0; i < 100; i++) { try { pid = Number(await readFile(pidFile, 'utf8')); break; } catch { await delay(10); } }
    assert.ok(pid); controller.abort(); await assert.rejects(pending, /aborted/);
    assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
  } finally { controller.abort(); await pending.catch(() => {}); await rm(directory, { recursive: true, force: true }); }
});
