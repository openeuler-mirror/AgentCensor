// Run using the package's Node against an independently extracted directory.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, readFile, writeFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const exec = promisify(execFile);
const bundle = resolve(process.argv[2]);
const directory = await mkdtemp(join(tmpdir(), 'cp-standalone-'));
const home = join(directory, 'home');
const config = join(directory, 'control');
await mkdir(home, { mode: 0o700 });
const env = { PATH: '/usr/bin:/bin', HOME: home, XDG_CONFIG_HOME: join(home, '.config'), LANG: 'C.UTF-8' };
const ctl = join(bundle, 'cpctl');
const checks = [];
const run = async (args, options = {}) => {
  const start = Date.now();
  const result = await exec(ctl, args, { env, cwd: directory, timeout: 180000, maxBuffer: 4 * 1024 * 1024, ...options });
  checks.push({ command: ['cpctl', ...args], passed: true, elapsed_ms: Date.now() - start });
  return result;
};
const call = (operation, ...args) => run([operation, '--config-dir', config, ...args]);
const requests = [];
const upstreamKey = 'fictional-standalone-provider-key';
const provider = createServer(async (request, response) => {
  try {
    assert.equal(request.url, '/v1/chat/completions');
    assert.equal(request.headers.authorization, 'Bearer ' + upstreamKey);
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks));
    requests.push(body);
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Connection', 'close');
    response.end(JSON.stringify({ id: 'standalone', object: 'chat.completion', choices: [{
      index: 0, message: { role: 'assistant', content: body.messages.at(-1).content }, finish_reason: 'stop',
    }] }));
  } catch {
    response.statusCode = 400;
    response.end('{"error":"invalid_fixture_request"}');
  }
});
await new Promise(ok => provider.listen(0, '127.0.0.1', ok));
const reserve = createServer();
await new Promise(ok => reserve.listen(0, '127.0.0.1', ok));
const port = reserve.address().port;
await new Promise(ok => reserve.close(ok));
let stopped = false;
try {
  for (const excluded of ['Cargo.toml', 'offline', 'tools', 'share/censorprivacy/scripts/model', '.venv', 'target']) {
    await assert.rejects(access(join(bundle, excluded)), { code: 'ENOENT' });
  }
  for (const binary of ['censorprivacy', 'privacy-run', 'privcalc', 'censorprivacy-mcp', 'tls-run']) {
    await exec(join(bundle, 'bin', binary), ['--help'], { env, cwd: directory, timeout: 10000 });
  }
  checks.push({ check: 'five_binaries_run_without_build_tools', passed: true });
  const selfTest = await run(['self-test']);
  console.log(selfTest.stdout);
  assert.match(selfTest.stdout, /完整交付包自检/);
  const key = join(directory, 'provider.key');
  await writeFile(key, upstreamKey, { mode: 0o600 });
  await call('init', '--upstream', 'http://127.0.0.1:' + provider.address().port,
    '--model-id', 'standalone-model', '--key-file', key, '--port', String(port));
  const secret = 'DEMO_SECRET_DO_NOT_SEND';
  await call('start');
  await call('check', '--model');
  assert.equal(requests.at(-1).thinking, undefined);
  const token = (await readFile(join(config, 'gateway.token'), 'utf8')).trim();
  const nerReady = await (await fetch('http://127.0.0.1:' + port + '/ready', {
    headers: { authorization: 'Bearer ' + token },
  })).json();
  assert.equal(nerReady.ner_device, 'cpu');
  assert.equal(nerReady.ner_batch_size, 1);
  const firstRun = JSON.parse(await readFile(join(config, 'run.json'))).run_id;
  const runPath = join(config, 'run.json');
  const runState = await readFile(runPath);
  await writeFile(runPath, JSON.stringify({ run_id: 'different-run' }));
  for (const args of [['check'], ['start'], ['xiaoo', 'setup']]) {
    await assert.rejects(call(...args), failure => /run ID 不匹配/.test(failure.stderr));
  }
  await writeFile(runPath, runState);
  const headersPath = join(config, 'curl.headers');
  const headers = await readFile(headersPath);
  await writeFile(headersPath, 'Authorization: Bearer invalid-fixture-token\n');
  for (const args of [['check'], ['start'], ['xiaoo', 'setup']]) {
    await assert.rejects(call(...args), failure => /认证失败（HTTP 401）/.test(failure.stderr));
  }
  await writeFile(headersPath, headers);
  checks.push({ check: 'check_start_xiaoo_distinguish_authentication_and_run_mismatch', passed: true });
  const text = await readFile(join(bundle, 'share/censorprivacy/examples/deepseek-restore.txt'), 'utf8');
  const protectedValues = ['张三', '李四', '清华大学', '北京大学', '13800138000', '13900139000',
    'zhangsan@example.com', 'lisi@example.org', '北京市海淀区中关村东路1号', '2026年11月5日', '860.00', '1240.00'];
  const response = await fetch('http://127.0.0.1:' + port + '/v1/chat/completions', {
    method: 'POST', headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'standalone-model', messages: [{ role: 'user', content: text }], stream: false }),
    signal: AbortSignal.timeout(90000),
  });
  const completion = await response.json();
  assert.equal(response.status, 200, JSON.stringify(completion.error));
  const displayed = completion.choices[0].message.content;
  const protectedText = JSON.stringify(requests.at(-1));
  for (const value of [...protectedValues, secret]) assert.ok(!protectedText.includes(value), 'raw value reached upstream: ' + value);
  assert.match(protectedText, /<CP_/);
  for (const value of protectedValues) assert.equal(displayed.split(value).length, text.split(value).length, 'authorized restore missing: ' + value);
  assert.ok(!displayed.includes(secret));
  assert.match(displayed, /REDACTED_SECRET/);
  checks.push({ check: 'configured_gateway_business_text_ner_phone_email_address_date_money_secret_and_restoration', passed: true });
  // A later assistant message can reveal the sensitivity of a value already
  // present in earlier history. Both the release and recorded detail must use
  // the complete request's sensitive values, while retaining final validation.
  const session = '0123456789abcdef'.repeat(2);
  const history = [{ role: 'user', content: 'Please retain the phrase hiddencredential for this conversation.' }];
  const chat = async messages => {
    const response = await fetch('http://127.0.0.1:' + port + '/xiaoo/' + session + '/v1/chat/completions', {
      method: 'POST', headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'standalone-model', messages, stream: false }),
      signal: AbortSignal.timeout(90000),
    });
    const body = await response.json();
    assert.equal(response.status, 200, JSON.stringify(body.error));
    return body;
  };
  await chat(history);
  history.push({role:'assistant',content:'password=hiddencredential'},
    {role:'user',content:'Summarize the preceding conversation.'});
  await chat(history);
  const historyWire = requests.at(-1);
  assert.ok(!JSON.stringify(historyWire).includes('hiddencredential'));
  assert.match(historyWire.messages[0].content, /<CP_SECRET_/);
  const recorded = await (await fetch('http://127.0.0.1:' + port + '/api/conversations/' + session)).json();
  assert.equal(recorded.exchanges.length, 2);
  assert.ok(recorded.exchanges.every(exchange => exchange.status === 'completed'));
  for (const snapshot of recorded.exchanges.at(-1).snapshots) {
    const index = Number(snapshot.path.split('/')[2]);
    assert.equal(snapshot.analysis.masked_text, historyWire.messages[index].content);
  }
  checks.push({check:'multi_turn_later_credentials_protect_earlier_history_and_exact_web_snapshots',passed:true});
  await call('stop');
  await call('start');
  assert.notEqual(JSON.parse(await readFile(join(config, 'run.json'))).run_id, firstRun);
  await call('check');
  await call('stop');
  // This standalone ships the CPU runtime. An explicit CUDA request must fail
  // before opening the listener, even on a host with NVIDIA hardware installed.
  for (const device of ['cuda', 'cann']) {
    await assert.rejects(call('start', '--ner-device', device, '--ner-batch-size', '4'));
    assert.match(await readFile(join(config, 'service.log'), 'utf8'), /ner_device_unavailable/);
  }
  await assert.rejects(fetch('http://127.0.0.1:' + port + '/ready', { signal: AbortSignal.timeout(1000) }));
  checks.push({ check: 'cpu_runtime_rejects_explicit_cuda_and_cann_without_silent_backend_fallback', passed: true });
  const policyPath = join(config, 'policy.yaml');
  const policy = await readFile(policyPath, 'utf8');
  assert.match(policy, /^ttl_seconds: \d+$/m);
  await writeFile(policyPath, policy.replace(/^ttl_seconds: \d+$/m, 'ttl_seconds: 3'));
  await call('start');
  const expiringRun = JSON.parse(await readFile(runPath)).run_id;
  const until = Date.now() + 10000;
  let expired = false;
  while (Date.now() < until) {
    const readiness = await fetch('http://127.0.0.1:' + port + '/ready', {
      headers: { authorization: 'Bearer ' + token }, signal: AbortSignal.timeout(2000),
    });
    if (readiness.status === 400 && (await readiness.json()).error?.code === 'vault_denied') {
      expired = true;
      break;
    }
    await new Promise(ok => setTimeout(ok, 100));
  }
  assert.ok(expired, 'real run must expire while the server still responds');
  for (const args of [['check'], ['start'], ['xiaoo', 'setup']]) {
    await assert.rejects(call(...args), failure => {
      assert.match(failure.stderr, /run 已失效.*vault_denied/);
      assert.match(failure.stderr, /cpctl stop.*cpctl start/);
      if (args[0] === 'start') assert.match(failure.stderr, /进程仍在运行/);
      assert.ok(!failure.stderr.includes('请先启动当前网关'));
      return true;
    });
  }
  // Expiration must also reject a model request without reaching the upstream.
  const beforeExpiryRequest = requests.length;
  const rejected = await fetch('http://127.0.0.1:' + port + '/v1/chat/completions', {
    method: 'POST', headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'standalone-model', messages: [{ role: 'user', content: 'OK' }] }),
    signal: AbortSignal.timeout(5000),
  });
  assert.equal(rejected.status, 400);
  assert.equal((await rejected.json()).error.code, 'vault_denied');
  assert.equal(requests.length, beforeExpiryRequest);
  await call('stop');
  await writeFile(policyPath, policy);
  await call('start');
  assert.notEqual(JSON.parse(await readFile(runPath)).run_id, expiringRun);
  await call('check');
  await call('stop');
  await assert.rejects(call('xiaoo', 'setup'), failure => /无法连接当前服务/.test(failure.stderr));
  checks.push({ check: 'real_ttl_expiry_fails_closed_diagnosed_by_check_start_xiaoo_and_restart_recovers', passed: true });
  stopped = true;
  const processRecord = JSON.parse(await readFile(join(config, 'process.json')));
  // A zombie may remain briefly under container PID 1, but it must not be serving.
  await assert.rejects(fetch('http://127.0.0.1:' + port + '/ready', { signal: AbortSignal.timeout(1000) }));
  checks.push({ check: 'restart_rotates_run_and_stop_closes_listener', pid: processRecord.pid, passed: true });
  console.log('ACCEPTANCE_JSON=' + JSON.stringify({ passed: true, platform: process.platform, uid: process.getuid(), os_release: await readFile('/etc/os-release', 'utf8'), package: bundle, inherited_detector_environment: false, checks }));
} finally {
  if (!stopped) await call('stop').catch(() => {});
  provider.closeAllConnections();
  await new Promise(ok => provider.close(ok));
  await rm(directory, { recursive: true, force: true });
}
