// Actual xiaoO binary + shipped Rust/ONNX gateway. No substitute agent client.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, readFile, writeFile, stat, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

process.umask(0o077);
const exec = promisify(execFile);
const bundle = resolve(process.argv[2]);
const binary = resolve(process.argv[3]);
const liveConfig = process.argv[4] ? resolve(process.argv[4]) : null;
const directory = await mkdtemp(join(tmpdir(), 'cp-xiaoo-'));
const home = join(directory, 'home'); await mkdir(home, { mode: 0o700 });
const config = join(directory, 'control');
const env = { PATH: '/usr/bin:/bin', HOME: home, XDG_CONFIG_HOME: join(home, '.config'), LANG: 'C.UTF-8' };
const ctl = join(bundle, 'cpctl');
const call = (...args) => {
  const pending = exec(ctl, [...args, '--config-dir', config], {
    env, timeout: 240000, maxBuffer: 4 * 1024 * 1024,
  });
  // xiaoO appends redirected stdin even with -p; an empty pipe needs EOF.
  pending.child.stdin.end();
  return pending;
};
const values = ['张三', '李四', '清华大学', '北京大学', '13800138000', '13900139000',
  'zhangsan@example.com', 'lisi@example.org', '北京市海淀区中关村东路1号', '2026年11月5日', '12800.00', '3200.00'];
const secret = 'DEMO_SECRET_DO_NOT_SEND';
const requests = [];
let provider, succeeded = false;
function send(response, payload, message) {
  if (payload.stream !== true) {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ choices: [{ index: 0, message, finish_reason: message.tool_calls ? 'tool_calls' : 'stop' }] }));
    return;
  }
  response.writeHead(200, { 'content-type': 'text/event-stream', connection: 'close' });
  const event = (delta, finish = null) => response.write('data: ' + JSON.stringify({
    id: 'xiaoo-fixture', object: 'chat.completion.chunk', created: 0, model: payload.model,
    choices: [{ index: 0, delta, finish_reason: finish }],
  }) + '\n\n');
  event({ role: 'assistant', content: '' });
  if (message.tool_calls) {
    const tool = message.tool_calls[0];
    event({ tool_calls: [{ index: 0, id: tool.id, type: 'function', function: { name: tool.function.name, arguments: '' } }] });
    for (const part of tool.function.arguments.match(/.{1,9}/gu)) {
      event({ tool_calls: [{ index: 0, function: { arguments: part } }] });
    }
  } else {
    // Split references across events as a real streaming provider does.
    for (const part of message.content.match(/[\s\S]{1,13}/gu) ?? []) event({ content: part });
  }
  event({}, message.tool_calls ? 'tool_calls' : 'stop');
  response.end('data: [DONE]\n\n');
}
try {
  let origin, model, credentialArgs;
  if (liveConfig) {
    const live = JSON.parse(await readFile(join(liveConfig, 'config.json')));
    assert.equal(live.mode, 'gateway'); assert.ok(live.upstream.startsWith('https://'));
    origin = live.upstream; model = live.model;
    credentialArgs = live.has_key ? ['--key-file', join(liveConfig, 'upstream.key')] : ['--no-key'];
  } else {
    provider = createServer(async (request, response) => {
      try {
        const chunks = []; for await (const chunk of request) chunks.push(chunk);
        const payload = JSON.parse(Buffer.concat(chunks)); requests.push(payload);
        assert.equal(request.url, '/v1/chat/completions');
        const tool = payload.messages.findLast(message => message.role === 'tool');
        const demo = payload.messages.some(message => message.role === 'user' && message.content.includes('file_read'));
        let message;
        if (demo && !tool) {
          assert.ok(payload.tools.some(tool => tool.function.name === 'file_read'));
          message = { role: 'assistant', content: null, tool_calls: [{ id: 'read_mail', type: 'function',
            function: { name: 'file_read', arguments: JSON.stringify({ file_path: 'privacy-business.txt' }) } }] };
        } else {
          const content = tool ? JSON.parse(tool.content).content : payload.messages.findLast(message => message.role === 'user').content;
          assert.equal(typeof content, 'string');
          message = { role: 'assistant', content };
        }
        send(response, payload, message);
      } catch (error) {
        response.writeHead(400, { 'content-type': 'application/json' }); response.end('{"error":"fixture_failed"}');
        await writeFile(join(directory, 'fixture-error.txt'), String(error.stack));
      }
    });
    await new Promise(ok => provider.listen(0, '127.0.0.1', ok));
    origin = 'http://127.0.0.1:' + provider.address().port; model = 'deepseek-flash'; credentialArgs = ['--no-key'];
  }
  const reserve = createServer(); await new Promise(ok => reserve.listen(0, '127.0.0.1', ok));
  const port = reserve.address().port; await new Promise(ok => reserve.close(ok));
  await call('init', '--upstream', origin, '--model-id', model, ...credentialArgs, '--port', String(port));
  await call('start');
  await call('xiaoo', 'setup', '--xiaoo-bin', binary);
  const token = (await readFile(join(config, 'gateway.token'), 'utf8')).trim();
  const generated = await readFile(join(config, 'xiaoo/config.toml'), 'utf8');
  assert.ok(generated.includes(`api_base = "http://127.0.0.1:${port}/v1/chat/completions"`));
  assert.ok(!generated.includes(token));
  assert.equal((await stat(join(config, 'xiaoo/config.toml'))).mode & 0o777, 0o600);
  const result = await call('xiaoo', 'demo'); // Reuses the saved executable path.
  await writeFile(join(directory, 'demo-stdout.txt'), result.stdout);
  await writeFile(join(directory, 'demo-stderr.txt'), result.stderr);
  for (const value of values) assert.ok(result.stdout.includes(value), 'restoration missing: ' + value);
  assert.ok(result.stdout.includes('[REDACTED_SECRET]'), 'SECRET must remain redacted');
  assert.ok(!result.stdout.includes(secret)); assert.ok(!result.stdout.includes(token));
  if (!liveConfig) {
    assert.ok(requests.length >= 2, 'file_read must execute and submit a tool result');
    assert.ok(requests.some(payload => payload.messages.some(message => message.role === 'tool')));
    assert.ok(requests.every(payload => payload.thinking?.type === 'disabled'));
    assert.ok(requests.every(payload => payload.stream === true), 'test must cover native xiaoO SSE');
    const wire = JSON.stringify(requests);
    for (const value of [...values, secret]) assert.ok(!wire.includes(value), 'raw value reached upstream: ' + value);
    assert.match(wire, /<CP_/);
    // A regular task and piped prompt use the same saved gateway configuration.
    const task = '请复述：联系人张三，电话13800138000。';
    const ordinary = await call('xiaoo', 'run', task);
    assert.ok(ordinary.stdout.includes('13800138000'));
    const piped = new Promise((ok, no) => {
      const child = execFile(ctl, ['xiaoo', 'run', '--config-dir', config], { env, timeout: 120000 },
        (error, stdout) => error ? no(error) : ok(stdout));
      child.stdin.end(task);
    });
    assert.ok((await piped).includes('13800138000'));
    await assert.rejects(call('xiaoo', 'run', '--api-base', 'http://127.0.0.1:1'));
    await assert.rejects(call('xiaoo', 'tui'), 'TUI must require an interactive terminal');
  }
  await call('check'); // Agent exit leaves the gateway running.
  const report = { passed: true, timestamp: new Date().toISOString(), actual_xiaoo_binary: binary,
    mode: liveConfig ? 'actual_remote_https_model' : 'local_fixture_with_actual_rust_onnx_and_xiaoo', model,
    checks: ['native_xiaoo_file_read_tool', 'long_mail_restoration', 'secret_stays_redacted', 'gateway_keeps_running'],
    ...(liveConfig ? {} : { upstream_requests: requests.length, extra_checks: ['native_sse', 'outbound_contains_no_raw_values',
      'deepseek_thinking_disabled', 'private_config_without_key', 'ordinary_task', 'stdin_task', 'reject_endpoint_override'] }),
  };
  if (process.env.CP_XIAOO_TEST_REPORT) await writeFile(process.env.CP_XIAOO_TEST_REPORT, JSON.stringify(report, null, 2) + '\n');
  console.log('PASS xiaoO: ' + JSON.stringify(report));
  succeeded = true;
} catch (error) {
  if (error.stdout) await writeFile(join(directory, 'failed-stdout.txt'), error.stdout);
  if (error.stderr) await writeFile(join(directory, 'failed-stderr.txt'), error.stderr);
  throw error;
} finally {
  await call('stop').catch(() => {});
  if (provider) { provider.closeAllConnections(); await new Promise(ok => provider.close(ok)); }
  if (succeeded) await rm(directory, { recursive: true, force: true });
  else { await writeFile(join(directory, 'mock-requests.json'), JSON.stringify(requests, null, 2)); console.log('Private diagnostics: ' + directory); }
}
