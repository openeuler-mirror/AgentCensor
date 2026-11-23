// Real censorprivacy + DSH, isolated synthetic HTTP endpoint. No model/cloud required.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';

process.umask(0o077);
const exec = promisify(execFile);
const root = resolve(import.meta.dirname, '../..');
const bundle = process.env.CP_TEST_BUNDLE ? resolve(process.env.CP_TEST_BUNDLE) : root;
const ctl = join(bundle, 'censorprivacy');
const dsh = resolve(process.argv[2] ?? '/root/deepseek-harness');
const directory = await mkdtemp(join(tmpdir(), 'cp-dsh-stream-'));
const config = join(directory, 'control');
const prefix = bundle === root ? join(root, 'target') : bundle;
const env = { PATH: process.env.PATH, HOME: directory, XDG_CONFIG_HOME: join(directory, '.config'), LANG: 'C.UTF-8' };
const args = ['--config-dir', config, '--prefix', prefix];
const requests = [], timings = [];
let responseEnded = false, current;
const chunk = (delta, finish_reason = null) => 'data: ' + JSON.stringify({ choices: [{ index: 0, delta, finish_reason }] }) + '\n\n';
const server = createServer(async (request, response) => {
  try {
    if (request.url === '/ready') {
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify({ status: 'ready', run_id: 'synthetic-stream-review', ner_threads: 4 })); return;
    }
    const chunks = []; for await (const bytes of request) chunks.push(bytes);
    const body = JSON.parse(Buffer.concat(chunks));
    requests.push(body);
    assert.equal(body.stream, true);
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    const toolResult = body.messages.some(message => message.role === 'tool');
    if (current === 'tool' && !toolResult) {
      response.write(chunk({ tool_calls: [{ index: 0, id: 'compute-audit', type: 'function', function: { name: 'private_compute', arguments: '{"operation":"sum","refs":[]}' } }] }));
      response.end(chunk({}, 'tool_calls') + 'data: [DONE]\n\n'); return;
    }
    const first = toolResult ? '工具完成，' : '第一段😀，';
    response.write(chunk({ role: 'assistant', content: first }));
    const timing = { first_sent: performance.now() }; timings.push(timing);
    await delay(1200);
    responseEnded = true;
    timing.ended = performance.now();
    response.end(chunk({ content: '第二段。' }) + chunk({}, 'stop') + 'data: [DONE]\n\n');
  } catch {
    response.destroy(new Error('fixture_failed'));
  }
});
await new Promise(ok => server.listen(0, '127.0.0.1', ok));
const origin = `http://127.0.0.1:${server.address().port}`;
const report = { passed: false, scope: 'real censorprivacy/DSH; synthetic local gateway without ONNX or cloud', modes: [] };
try {
  await exec(ctl, ['init', '--upstream', origin, '--model-id', 'deepseek-flash', '--no-key', '--port', String(server.address().port), ...args], { env });
  await writeFile(join(config, 'run.json'), JSON.stringify({ run_id: 'synthetic-stream-review' }), { mode: 0o600 });
  for (const mode of ['text', 'tool']) {
    current = mode; responseEnded = false;
    const child = spawn(ctl, ['dsh', 'run', '请回复测试文本。', '--dsh-root', dsh, ...args], { env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '', stderr = '', sawEarlyText = false, firstSeen;
    child.stdout.on('data', chunk => {
      stdout += chunk;
      if (stdout.includes(mode === 'text' ? '第一段😀，' : '工具完成，') && !responseEnded) {
        sawEarlyText = true; firstSeen ??= performance.now();
      }
    });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const timer = setTimeout(() => child.kill('SIGTERM'), 30000);
    const code = await new Promise((ok, no) => { child.once('error', no); child.once('close', ok); });
    clearTimeout(timer);
    assert.equal(code, 0, stderr);
    assert.ok(sawEarlyText, 'text must reach censorprivacy stdout before response completion');
    const expected = mode === 'text' ? '第一段😀，第二段。\n' : '工具完成，第二段。\n';
    assert.equal(stdout, expected, 'native headless final summary must not be printed twice');
    const timing = timings.at(-1);
    report.modes.push({ mode, first_text_after_fixture_write_ms: firstSeen - timing.first_sent,
      first_text_before_end_ms: timing.ended - firstSeen, no_duplicate_summary: true });
  }
  assert.equal(requests.length, 3);
  assert.ok(requests.every(body => body.stream && body.thinking.type === 'disabled'));
  report.passed = true;
  if (process.env.CP_DSH_STREAM_REPORT) await writeFile(process.env.CP_DSH_STREAM_REPORT, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report));
} finally {
  server.closeAllConnections(); await new Promise(ok => server.close(ok));
  await rm(directory, { recursive: true, force: true });
}
