// Real Rust processes with a local deterministic provider, plus isolated DSH profiles.
import { createServer as httpServer } from 'node:http';
import { createServer as httpsServer } from 'node:https';
import { mkdtemp, mkdir, lstat, readFile, access, rm, open } from 'node:fs/promises';
import { constants } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { tmpdir, homedir } from 'node:os';
import { randomBytes, createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';
import assert from 'node:assert/strict';
import { requestJSON, tlsJSON } from '../../plugins/dsh-censorprivacy/transport.mjs';

process.umask(0o077);
const positionals = []; const flags = {};
for (let i = 2; i < process.argv.length; i++) {
  if (process.argv[i] === '--') { positionals.push(...process.argv.slice(i + 1)); break; }
  if (['--root', '--bin-dir', '--config-dir', '--dsh-root', '--native-library'].includes(process.argv[i])) flags[process.argv[i].slice(2)] = process.argv[++i];
  else positionals.push(process.argv[i]);
}
const [operation, action, ...taskParts] = positionals;
const root = flags.root; const bin = flags['bin-dir']; const configDir = flags['config-dir'];
const children = new Set(); const servers = new Set(); const temporaries = new Set();
const customer = 'DemoCustomerOrchid'; const secret = 'DEMO_SECRET_DO_NOT_SEND';
const input = `Customer=${customer}; amountA=100.00元; amountB=200.00元; secret=${secret}.`;
const task = `Use private_compute exactly once to sum the two amount references. Reply with the customer name and total, without code or links. ${input}`;
const log = message => console.log(message);
const assertion = (condition, message) => { if (!condition) throw new Error(message); };
async function exists(path) { try { await access(path); return true; } catch { return false; } }
async function writeFile(path, value) {
  const file = await open(path, constants.O_WRONLY | constants.O_CREAT | constants.O_NOFOLLOW | constants.O_NONBLOCK, 0o600);
  try {
    const info = await file.stat();
    assertion(info.isFile() && !(info.mode & 0o077) && info.uid === process.getuid(), 'private_output_required');
    await file.truncate(0); await file.writeFile(value);
  } finally { await file.close(); }
}
async function privateDirectory(path) {
  await mkdir(path, { mode: 0o700 }).catch(failure => { if (failure.code !== 'EEXIST') throw failure; });
  const info = await lstat(path);
  assertion(info.isDirectory() && !(info.mode & 0o077) && info.uid === process.getuid(), 'private_directory_required');
}
async function privateText(path) {
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await file.stat();
    assertion(info.isFile() && !(info.mode & 0o077) && info.uid === process.getuid() && info.size <= 1024 * 1024, 'private_file_required');
    return await file.readFile('utf8');
  } finally { await file.close(); }
}
async function temporary() {
  const directory = await mkdtemp(join(tmpdir(), 'cp-demo-')); temporaries.add(directory); return directory;
}
function start(command, args, options = {}) {
  const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'], ...options });
  children.add(child); child.on('close', () => children.delete(child));
  child.once('error', () => { child.cpLaunchFailed = true; });
  return child;
}
async function run(command, args, options = {}) {
  const child = start(command, args, options);
  const stdout = []; const stderr = []; let bytes = 0;
  for (const [stream, chunks] of [[child.stdout, stdout], [child.stderr, stderr]]) {
    stream?.on('data', chunk => { bytes += chunk.length; if (bytes > 4 * 1024 * 1024) child.kill('SIGTERM'); else chunks.push(chunk); });
  }
  const timeout = setTimeout(() => child.kill('SIGTERM'), options.timeout ?? 120000);
  return await new Promise((resolveResult, reject) => {
    child.once('error', () => { clearTimeout(timeout); reject(new Error('command_unavailable')); });
    child.once('close', (code, signal) => {
      clearTimeout(timeout);
      const result = { code, signal, stdout: Buffer.concat(stdout).toString(), stderr: Buffer.concat(stderr).toString() };
      if (code !== 0 && !options.allowFailure) {
        // DSH may include request text in its errors; retain only in the private workspace.
        reject(Object.assign(new Error('command_failed'), { diagnostic: result }));
      } else resolveResult(result);
    });
  });
}
async function listen(server) {
  servers.add(server);
  await new Promise((ok, no) => { server.once('error', no); server.listen(0, '127.0.0.1', ok); });
  return server.address().port;
}
async function freePort() {
  const server = httpServer(); const port = await listen(server);
  await new Promise(ok => server.close(ok)); servers.delete(server); return port;
}
async function certificates(directory) {
  await run('openssl', ['req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes',
    '-keyout', join(directory, 'server.key'), '-out', join(directory, 'server.crt'), '-subj', '/CN=localhost',
    '-addext', 'subjectAltName=IP:127.0.0.1', '-days', '1']);
}
function completion(content, calls) {
  return { id: 'cp-demo-response', object: 'chat.completion', choices: [{ index: 0,
    message: { role: 'assistant', content, ...(calls ? { tool_calls: calls } : {}) },
    finish_reason: calls ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } };
}
const fields = text => ({ customer: text.match(/Customer=([^;]+)/)?.[1]?.trim(),
  amounts: [...text.matchAll(/amount[AB]=([^;]+)/g)].map(match => match[1].trim().replace(/元$/, '')) });
async function mock(directory, tls, agent) {
  const captured = [];
  const handler = async (request, response) => {
    try {
      const chunks = []; let size = 0;
      for await (const chunk of request) { size += chunk.length; if (size > 256 * 1024) throw new Error('request_limit'); chunks.push(chunk); }
      const payload = JSON.parse(Buffer.concat(chunks));
      captured.push(payload);
      await writeFile(join(directory, 'mock-requests.json'), JSON.stringify(captured, null, 2), { mode: 0o600 });
      if (request.url !== '/v1/chat/completions' || request.method !== 'POST') throw new Error('route_invalid');
      const userText = payload.messages.filter(message => message.role === 'user').map(message => message.content).join('\n');
      const values = fields(userText);
      const tool = payload.messages.findLast(message => message.role === 'tool');
      let reply;
      if (!agent) reply = completion(payload.messages.at(-1).content);
      else if (!tool) {
        assertion(values.amounts.length === 2 && payload.tools?.some(tool => tool.function.name === 'private_compute'), 'private_compute_not_available');
        reply = completion(null, [{ id: 'cp_demo_sum', type: 'function', function: { name: 'private_compute',
          arguments: JSON.stringify({ operation: 'sum', refs: values.amounts }) } }]);
      } else {
        const result = JSON.parse(tool.content);
        assertion(typeof result.results?.sum === 'string', 'derived_reference_missing');
        reply = completion(`Customer ${values.customer}; total ${result.results.sum}; secret [REDACTED_SECRET].`);
      }
      const wire = Buffer.from(JSON.stringify(reply));
      response.writeHead(200, { 'content-type': 'application/json', 'content-length': wire.length, connection: 'close' });
      response.end(wire);
    } catch (failure) {
      await writeFile(join(directory, 'mock-error.txt'), String(failure.message), { mode: 0o600 });
      response.writeHead(400, { 'content-type': 'application/json', connection: 'close' });
      response.end('{"error":"mock_request_invalid"}');
    }
  };
  if (tls) await certificates(directory);
  const server = tls ? httpsServer({ key: await readFile(join(directory, 'server.key')), cert: await readFile(join(directory, 'server.crt')) }, handler) : httpServer(handler);
  const port = await listen(server);
  return { server, captured, origin: `${tls ? 'https' : 'http'}://127.0.0.1:${port}` };
}
async function ready(base, token, child, runFile) {
  for (let i = 0; i < 100; i++) {
    assertion(!child.cpLaunchFailed && child.exitCode === null && child.signalCode === null, 'rust_gateway_exited');
    try {
      const response = await fetch(`${base}/ready`, { headers: { authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(500) });
      const value = await response.json();
      if (response.ok && value.status === 'ready' && value.run_id === JSON.parse(await readFile(runFile)).run_id) return value;
    } catch { /* listener/run file may still be starting; deadline is bounded */ }
    await delay(50);
  }
  throw new Error('gateway_readiness_timeout');
}
async function nativeLibrary() {
  let library = flags['native-library'];
  if (!library && await exists(join(configDir, 'install.json'))) library = JSON.parse(await privateText(join(configDir, 'install.json'))).native_library;
  if (!library && await exists(join(bin, '../lib/censorprivacy/libcp_tls_guard.so'))) library = join(bin, '../lib/censorprivacy/libcp_tls_guard.so');
  library ||= join(root, 'native/libcp_tls_guard.so');
  if (!await exists(library)) throw new Error('native_library_missing_run_cpctl_install_--tls');
  return resolve(library);
}
async function fixture(mode, agent, actual) {
  const directory = await temporary(); const workspace = join(directory, 'workspace'); await mkdir(workspace, { mode: 0o700 });
  const provider = actual ? { origin: actual.upstream } : await mock(directory, mode === 'tls', agent);
  const token = randomBytes(32).toString('hex'); const tokenFile = join(directory, 'gateway.token');
  const runFile = join(directory, 'run.json');
  await writeFile(tokenFile, token, { mode: 0o600 });
  await writeFile(join(directory, 'policy.yaml'), `epoch: 1\nmask_money: true\nmask_dates: true\ndictionaries:\n  CUSTOMER: [${customer}]\n  SECRET: [${secret}]\n`, { mode: 0o600 });
  const port = await freePort(); const base = `http://127.0.0.1:${port}`;
  const model = actual?.model ?? 'cp-demo';
  const args = ['serve', '--upstream', provider.origin, '--model', model, '--token-file', tokenFile,
    '--run-state-file', runFile, '--policy', join(directory, 'policy.yaml'), '--host', '127.0.0.1', '--port', String(port)];
  if (mode === 'tls') args.push('--tls-direct');
  if (provider.origin.startsWith('http:')) args.push('--allow-http-test-upstream');
  if (actual?.has_key) args.push('--upstream-key-file', join(configDir, 'upstream.key'));
  if (actual?.ca_file) args.push('--upstream-ca-file', actual.ca_file);
  const child = start(join(bin, 'censorprivacy'), args);
  let stderr = ''; child.stderr.on('data', chunk => { if (stderr.length < 8192) stderr += chunk; }); child.stdout.resume();
  try { await ready(base, token, child, runFile); } catch (failure) { await writeFile(join(directory, 'gateway.log'), stderr); throw failure; }
  const config = { mode, baseURL: mode === 'tls' ? provider.origin : base, broker: base, model,
    tlsRun: join(bin, 'tls-run'), nativeLibrary: mode === 'tls' ? (actual?.native_library || await nativeLibrary()) : '', tokenFile,
    runStateFile: runFile, workspace, caFile: mode === 'tls' ? (actual?.ca_file ?? (actual ? '' : join(directory, 'server.crt'))) : '',
    upstreamKeyFile: mode === 'tls' && actual?.has_key ? join(configDir, 'upstream.key') : '',
    receiptFile: join(directory, 'receipts.jsonl'), timeoutMs: 90000, contextWindow: 32768, maxTokens: 512 };
  const send = body => mode === 'tls' ? tlsJSON(config, body) : requestJSON(`${base}/v1/chat/completions`, body, { token });
  return { directory, workspace, provider, token, base, config, send };
}
function assertProtected(captured) {
  assertion(captured.length > 0, 'provider_received_no_requests');
  const wire = JSON.stringify(captured);
  for (const value of [customer, secret, '100.00', '200.00']) assertion(!wire.includes(value), 'raw_value_reached_mock_provider');
}
async function directDemo(mode) {
  log(`本机 ${mode} 演示：启动真实 Rust 网关和本机模拟上游（无云调用）`);
  const f = await fixture(mode, false);
  const body = content => ({ model: 'cp-demo', messages: [{ role: 'user', content }], stream: false });
  const response = await f.send(body(input));
  const displayed = response.choices[0].message.content;
  assertProtected(f.provider.captured);
  assertion(displayed.includes(customer) && displayed.includes('100.00') && displayed.includes('200.00') && !displayed.includes(secret), 'display_restore_failed');
  log(`原始输入（虚构）：${input}`);
  log(`上游收到：${f.provider.captured[0].messages[0].content}`);
  log(`本机显示：${displayed}`);
  const amounts = fields(f.provider.captured[0].messages[0].content).amounts;
  const result = await requestJSON(`${f.base}/v1/private_compute`, { operation: 'sum', refs: amounts }, { token: f.token });
  assertion(/^<CP_/.test(result.results?.sum), 'compute_did_not_return_reference');
  log(`私密计算返回：${JSON.stringify(result)}`);
  const derived = await f.send(body(`Total ${result.results.sum}`));
  assertion(derived.choices[0].message.content.includes('300'), 'derived_display_failed');
  log(`派生结果本机显示：${derived.choices[0].message.content}`);
  assertion((await fetch(`${f.base}/ready`)).status === 401, 'authentication_not_enforced');
  const before = f.provider.captured.length;
  await assert.rejects(f.send({ ...body('test'), metadata: { salary: 123 } }));
  assertion(f.provider.captured.length === before, 'blocked_request_reached_provider');
  assertProtected(f.provider.captured);
  if (mode === 'tls') {
    const status = await fetch(`${f.base}/v1/tls/status`, { headers: { authorization: `Bearer ${f.token}` } }).then(response => response.json());
    log(`TLS broker 决策计数：${JSON.stringify(status.counts)}`);
  }
  log('通过：上游无原值、授权显示、SECRET 遮盖、派生引用、认证及非法数值请求拒绝。');
}
async function dshCommand(givenRoot) {
  if (givenRoot) {
    const entry = join(resolve(givenRoot), 'apps/cli/lib/bin.js');
    assertion(await exists(entry), 'dsh_root_not_built');
    return [process.execPath, entry];
  }
  return ['dsh'];
}
async function overlay(directory, config) {
  const content = [
    '- insert:', '    - id: censorprivacy', `      name: ${JSON.stringify(join(root, 'plugins/dsh-censorprivacy/index.mjs'))}`, '      config:',
    ...Object.entries({ ...config, privateToolsOnly: true }).map(([key, value]) => `        ${key}: ${JSON.stringify(value)}`),
    '- id: agent-default-model', '  config:', '    provider: censorprivacy', `    model: ${JSON.stringify(config.model)}`,
    ...['llm-deepseek', 'llm-pi-ai', 'session-title-llm', 'session-log-deepseek', 'session-telemetry-otel'].flatMap(id => [`- id: ${id}`, '  disabled: true']),
  ].join('\n') + '\n';
  const path = join(directory, 'censorprivacy.patch.yml'); await writeFile(path, content, { mode: 0o600 }); return path;
}
function dshEnvironment(home, token, runID) {
  // Never inherit real provider credentials, telemetry settings or preload into DSH.
  return { PATH: process.env.PATH ?? '/usr/bin:/bin', HOME: homedir(), LANG: process.env.LANG ?? 'C.UTF-8',
    DSH_HOME: home, CENSORPRIVACY_TOKEN: token, CENSORPRIVACY_RUN_ID: runID };
}
async function dshProfile(directory, config, token, dshRoot, persistent = false) {
  const home = join(directory, 'dsh-home'); await mkdir(home, { recursive: true, mode: 0o700 });
  const command = await dshCommand(dshRoot);
  const patch = await overlay(directory, config);
  const env = dshEnvironment(home, token, JSON.parse(await privateText(config.runStateFile)).run_id);
  const profile = 'censorprivacy'; const manifest = join(home, 'profiles', profile, 'package.json');
  if (!await exists(manifest)) {
    const result = await run(command[0], [...command.slice(1), '--profile', profile, '--from-default-profile', 'headless', '--patch', patch, '--dump-config'], { env, cwd: config.workspace });
    await writeFile(join(directory, 'dsh-composed.yaml'), result.stdout, { mode: 0o600 });
  }
  const invoke = async prompt => {
    // Both the DSH launcher and the headless app parse argv. End each one's
    // options so arbitrary task text cannot select a profile or print help.
    const result = await run(command[0], [...command.slice(1), '--profile', profile, '--patch', patch, '--', '--', prompt], { env, cwd: config.workspace, allowFailure: true, timeout: 180000 });
    await writeFile(join(directory, 'dsh-stdout.txt'), result.stdout, { mode: 0o600 });
    await writeFile(join(directory, 'dsh-stderr.txt'), result.stderr, { mode: 0o600 });
    assertion(result.code === 0, `dsh_agent_failed_see_${join(directory, 'dsh-stderr.txt')}`);
    return result.stdout.trim();
  };
  if (persistent) await writeFile(join(directory, 'setup.json'), JSON.stringify({ dshRoot, workspace: config.workspace }), { mode: 0o600 });
  return invoke;
}
async function dshDemo(mode) {
  log(`DSH ${mode} 演示：独立 profile + 真实 headless agent + 本机模拟上游`);
  const f = await fixture(mode, true);
  const invoke = await dshProfile(f.directory, f.config, f.token, flags['dsh-root']);
  const reply = await invoke(task);
  assertProtected(f.provider.captured);
  assertion(f.provider.captured.length >= 2 && f.provider.captured.some(body => body.messages.some(message => message.role === 'tool')), 'dsh_private_compute_not_executed');
  assertion(reply.includes(customer) && reply.includes('300') && !reply.includes(secret), 'dsh_final_reply_invalid');
  const receipts = (await privateText(f.config.receiptFile)).trim().split('\n').map(line => JSON.parse(line));
  assertion(receipts.some(event => event.event === 'private_compute' && event.arguments_are_references && event.results_are_references), 'compute_receipt_missing');
  log(`上游收到：${f.provider.captured[0].messages.filter(message => message.role === 'user').map(message => message.content).join('\n')}`);
  log(`DSH 最终回复：${reply}`);
  log('通过：实际 DSH 插件加载、模型适配、private_compute 调用和受保护结果显示。');
}
async function managedDsh() {
  assertion(['setup', 'run', 'cloud'].includes(action), 'use_dsh_setup_run_or_cloud');
  await privateDirectory(configDir);
  const config = JSON.parse(await privateText(join(configDir, 'config.json')));
  const directory = join(configDir, 'dsh'); await privateDirectory(directory);
  const workspace = join(directory, 'workspace'); await privateDirectory(workspace);
  const base = `http://127.0.0.1:${config.port}`;
  const token = (await privateText(join(configDir, 'gateway.token'))).trim();
  const runStateFile = join(configDir, 'run.json');
  const readyResponse = await fetch(`${base}/ready`, { headers: { authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(3000) });
  const readyValue = await readyResponse.json();
  assertion(readyResponse.ok && readyValue.run_id === JSON.parse(await privateText(runStateFile)).run_id, 'start_current_gateway_first');
  if (action === 'cloud') {
    const target = new URL(config.upstream);
    assertion(target.protocol === 'https:' && !target.hostname.endsWith('.example') && !/^(?:127\.|0\.)/.test(target.hostname) && !['localhost', '::1', '[::1]'].includes(target.hostname), 'actual_remote_https_upstream_required');
    const reportPath = join(directory, 'cloud-report.json');
    const report = { status: 'running', mode: config.mode, model: config.model, upstream: config.upstream, timestamp: new Date().toISOString(),
      scope: 'Actual DSH agent and remote HTTPS model, isolated synthetic policy and run. Cloud internal storage is not observable.' };
    await writeFile(reportPath, JSON.stringify(report, null, 2));
    try {
      // A separate run with fictional rules avoids editing the live business policy.
      const f = await fixture(config.mode, true, config);
      const invoke = await dshProfile(f.directory, f.config, f.token, flags['dsh-root'] ||
        (await exists(join(directory, 'setup.json')) ? JSON.parse(await privateText(join(directory, 'setup.json'))).dshRoot : ''));
      const reply = await invoke(task + ' You must call private_compute with operation sum and only the two provided amount references. Echo the protected customer reference exactly and the derived sum reference exactly in your final plain text. Do not guess values.');
      const receipts = (await privateText(f.config.receiptFile)).trim().split('\n').map(line => JSON.parse(line));
      assertion(receipts.filter(event => event.event === 'model_response').length >= 2 && receipts.some(event => event.event === 'private_compute' && event.operation === 'sum' && event.ref_count === 2), 'cloud_private_compute_not_confirmed');
      assertion(reply.includes(customer) && /\b300(?:\.00)?\b/.test(reply) && !reply.includes(secret), 'cloud_authorized_display_not_confirmed');
      report.status = 'passed'; report.run_id = JSON.parse(await privateText(f.config.runStateFile)).run_id;
      report.checks = ['remote_model_response', 'DSH_private_compute_with_references', 'derived_results_are_references', 'authorized_customer_and_sum_display', 'secret_not_displayed'];
      report.receipts = receipts; report.reply_sha256 = createHash('sha256').update(reply).digest('hex');
      log(`DSH 真实上游最终显示（虚构数据）：${reply}`);
      log('通过：真实模型工具调用、引用计算及授权显示；请求出口保护的逐字节证据见本机模拟验收。');
    } catch (failure) {
      report.status = 'failed'; report.reason = 'cloud_acceptance_failed'; throw failure;
    } finally { await writeFile(reportPath, JSON.stringify(report, null, 2), { mode: 0o600 }); log(`云端验收记录：${reportPath}`); }
    return;
  }
  const transport = { mode: config.mode, baseURL: config.mode === 'tls' ? config.upstream : base, model: config.model,
    broker: base, tlsRun: join(bin, 'tls-run'), nativeLibrary: config.native_library, tokenFile: join(configDir, 'gateway.token'),
    runStateFile, upstreamKeyFile: config.has_key ? join(configDir, 'upstream.key') : '', caFile: config.ca_file ?? '', workspace,
    timeoutMs: 90000, contextWindow: 32768, maxTokens: 512 };
  let dshRoot = flags['dsh-root'];
  if (!dshRoot && await exists(join(directory, 'setup.json'))) dshRoot = JSON.parse(await privateText(join(directory, 'setup.json'))).dshRoot;
  const invoke = await dshProfile(directory, transport, token, dshRoot, true);
  if (action === 'setup') { log(`DSH 独立 profile 就绪：${directory}；./cpctl dsh run '任务'`); return; }
  if (action === 'run') { assertion(taskParts.length > 0, 'task_required'); log(await invoke(taskParts.join(' '))); return; }
  throw new Error('use_dsh_setup_run_or_cloud');
}
let cleanupPromise;
function cleanup() { return cleanupPromise ??= cleanupAll(); }
async function cleanupAll() {
  for (const child of children) child.kill('SIGTERM');
  for (const server of servers) { server.closeAllConnections(); server.close(); }
  await delay(100);
  await Promise.all([...children].map(child => new Promise(ok => {
    child.once('close', ok); child.kill('SIGKILL');
    if (child.exitCode !== null || child.signalCode !== null) ok();
  })));
  for (const directory of temporaries) await rm(directory, { recursive: true, force: true });
}
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => { cleanup().finally(() => process.exit(signal === 'SIGINT' ? 130 : 143)); });
try {
  if (operation === 'dsh') await managedDsh();
  else if (['gateway', 'tls'].includes(action)) await directDemo(action);
  else if (['dsh-gateway', 'dsh-tls'].includes(action)) await dshDemo(action.slice(4));
  else throw new Error('use_demo_gateway_tls_dsh-gateway_or_dsh-tls');
} catch (failure) {
  console.error(`演示/验收失败：${failure.message}`);
  // On failure preserve private diagnostics for local inspection, never dump payloads.
  for (const directory of temporaries) { log(`私有诊断目录：${directory}`); }
  temporaries.clear();
  process.exitCode = 1;
} finally { await cleanup(); }
