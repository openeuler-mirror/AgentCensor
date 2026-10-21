import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { serialize, privateResult, PrivacyAdapter } from '../plugins/dsh-censorprivacy/index.mjs';
import { requestJSON, tlsJSON } from '../plugins/dsh-censorprivacy/transport.mjs';

const reference = '<CP_MONEY_0123456789abcdef0123456789abcdef>';
test('DSH rejects unsupported content and never returns raw compute results or extra fields', () => {
  const options = { model: 'model', messages: [{ role: 'user', content: [{ type: 'text', text: 'Hello' }] }] };
  assert.equal(serialize(options, { maxTokens: 512 }).stream, false);
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
test('TLS cancellation waits for the client process to close', { timeout: 10000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-plugin-test-')); const executable = join(directory, 'client'); const pidFile = join(directory, 'pid');
  await writeFile(executable, `#!/usr/bin/env node\nimport('node:fs').then(fs => fs.writeFileSync(${JSON.stringify(pidFile)}, String(process.pid)));\nsetInterval(() => {}, 1000);\n`, { mode: 0o700 });
  const controller = new AbortController();
  const pending = tlsJSON({ baseURL: 'https://127.0.0.1', tlsRun: executable, broker: 'http://127.0.0.1:1', tokenFile: '/unused', runStateFile: '/unused', nativeLibrary: '/unused', workspace: directory }, {}, controller.signal);
  try {
    let pid;
    for (let i = 0; i < 100; i++) { try { pid = Number(await readFile(pidFile, 'utf8')); break; } catch { await delay(10); } }
    assert.ok(pid); controller.abort(); await assert.rejects(pending, /aborted/);
    assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
  } finally { controller.abort(); await pending.catch(() => {}); await rm(directory, { recursive: true, force: true }); }
});
