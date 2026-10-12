#!/usr/bin/env node
// Real Chromium + shipped DSH Web + 0.5.0 BPF daemon, with isolated state and a local model stub.
// Usage: node scripts/test/integration-dsh-web.mjs /path/to/deepseek-harness
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createServer as createPortProbe } from 'node:net';
import { mkdtempSync, mkdirSync, writeFileSync, readdirSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { spawn, execFileSync } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const dsh = resolve(process.argv[2] ?? process.env.DSH_REPO ?? '/root/deepseek-harness');
const plugin = resolve(process.argv[3] ?? join(repo, 'plugins/dsh-censorguard'));
const requireDsh = createRequire(join(dsh, 'package.json'));
const requireWeb = createRequire(join(dsh, 'apps/web/package.json'));
const { chromium } = requireWeb('playwright');
const directory = mkdtempSync(join(tmpdir(), 'cg-web-'));
const home = join(directory, 'home');
const profile = join(home, 'profiles/web');
const workspace = join(directory, 'workspace');
const config = join(directory, 'base.yaml');
mkdirSync(workspace);
mkdirSync(join(profile, 'node_modules/@censorguard'), { recursive: true });
for (const entry of readdirSync(join(dsh, 'apps/cli/node_modules'))) {
  if (entry !== '@censorguard') symlinkSync(join(dsh, 'apps/cli/node_modules', entry), join(profile, 'node_modules', entry));
}
symlinkSync(plugin, join(profile, 'node_modules/@censorguard/dsh'));
writeFileSync(join(profile, 'package.json'), JSON.stringify({
  private: true, dsh: { profile: { bundles: ['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app', '@censorguard/dsh'] } },
  dependencies: { '@censorguard/dsh': `link:${plugin}` },
}));
console.log(`Artifacts: ${directory}`);
const children = [];
let browser;
let modelRequests = 0;
const model = createServer(async (request, response) => {
  if (!request.url.endsWith('/chat/completions')) { response.writeHead(404).end(); return; }
  for await (const _chunk of request) { /* Drain the local test request without retaining prompts. */ }
  modelRequests++;
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const chunk = (delta, finish_reason = null) => `data: ${JSON.stringify({id: 'cg-web-test', object: 'chat.completion.chunk', created: 1, model: 'deepseek-flash', choices: [{index: 0, delta, finish_reason}]})}\n\n`;
  response.end(chunk({ role: 'assistant', content: 'CensorGuard Web test reply.' }) + chunk({}, 'stop') + 'data: [DONE]\n\n');
});

function run(binary, args) {
  return execFileSync(join(repo, 'target/debug', binary), args, { encoding: 'utf8', timeout: 40000 });
}
function ctl(...args) {
  if (['status', 'doctor'].includes(args[0])) args.push('--json');
  return JSON.parse(run('censorguardctl', ['--config', config, ...args]));
}
function launch(command, args, label, options = {}) {
  let log = '';
  const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'], detached: true, ...options });
  children.push(child);
  const logPath = join(directory, `${label}.log`);
  for (const stream of [child.stdout, child.stderr]) stream.on('data', data => { log += data; writeFileSync(logPath, log); });
  return { child, log: () => log };
}
async function waitFor(check, message, timeout = 30000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const result = await check(); if (result) return result; await sleep(100); }
  throw new Error(message);
}
async function freePort() {
  const probe = createPortProbe();
  await new Promise((resolvePort, reject) => { probe.once('error', reject); probe.listen(0, '127.0.0.1', resolvePort); });
  const port = probe.address().port;
  await new Promise(resolveClose => probe.close(resolveClose));
  return port;
}

try {
  await new Promise((resolveListen, reject) => { model.once('error', reject); model.listen(0, '127.0.0.1', resolveListen); });
  const grpcPort = await freePort();
  const secret = join(workspace, 'guarded.txt');
  writeFileSync(secret, 'protected test content\n');
  writeFileSync(join(workspace, 'public.txt'), 'public test content\n');
  writeFileSync(config, `daemon:
  bpf_object: ${repo}/bpf/enforce.bpf.o
  control_socket: ${directory}/ctl.sock
  event_socket: ${directory}/events.sock
  launch_socket: ${directory}/launch.sock
  dsh_socket: ${directory}/dsh.sock
  ui_socket: ${directory}/ui.sock
  state_dir: ${directory}/state
  spawn_grpc: true
  grpc_bin: ${repo}/target/debug/censorguard-grpc
  grpc_listen: 127.0.0.1:${grpcPort}
rules:
  - file deny+audit ${secret}
`);
  const daemon = launch(join(repo, 'target/debug/censorguardd'), ['launch', '--config', config], 'daemon');
  await waitFor(() => {
    assert.equal(daemon.child.exitCode, null, daemon.log());
    return daemon.log().includes('required hooks attached');
  }, 'daemon not ready');
  const policy = join(directory, 'group.yaml');
  writeFileSync(policy, 'rules: []\n');
  ctl('policy', 'apply', '--policy', 'censorguard-dsh-default', '--file', policy);
  writeFileSync(join(profile, 'cordis.patch.yml'), `
- id: censorguard-bootstrap
  config:
    policyGroup: censorguard-dsh-default
    dshSockPath: ${directory}/dsh.sock
    heartbeatIntervalMs: 200
- id: censorguard-host
  config:
    grpcAddr: 127.0.0.1:${grpcPort}
    boundGroup: censorguard-dsh-default
- id: session-title-llm
  disabled: true
- id: llm-deepseek
  config:
    apiKeyEnv: CENSORGUARD_WEB_TEST_API_KEY
    baseURL: http://127.0.0.1:${model.address().port}
`);
  const web = launch(process.execPath, ['--import', requireDsh.resolve('tsx/esm'), join(dsh, 'apps/cli/src/bin.ts'), '--profile', 'web', '--port', '0', '--no-open'], 'host', {
    cwd: dsh, env: { ...process.env, DSH_HOME: home, DSH_TELEMETRY_DISABLED: '1', CENSORGUARD_WEB_TEST_API_KEY: 'unused-local-test-key' },
  });
  const url = await waitFor(() => {
    assert.equal(web.child.exitCode, null, web.log().slice(-7000));
    return web.log().match(/dsh web: (http:\/\/\S+)/)?.[1];
  }, 'DSH Web not ready', 60000);
  assert(web.log().includes('attach 成功'), 'DSH must actually be protected');
  assert(!web.log().includes('not found'), 'bundle must target current DSH entries');
  browser = await chromium.launch(process.env.DSH_PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.DSH_PLAYWRIGHT_EXECUTABLE_PATH } : {});
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1680, height: 1000 } });
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  await page.goto(url);
  await page.getByRole('button', { name: '继续', exact: true }).click();
  await page.getByRole('button', { name: '设置', exact: true }).click();
  const settings = page.getByRole('dialog', { name: '设置', exact: true });
  await settings.getByRole('button', { name: '安全策略', exact: true }).click();
  await settings.getByText('已连接', { exact: true }).waitFor();
  assert((await settings.locator('textarea').inputValue()).includes('rules:'));
  writeFileSync(join(directory, 'settings.txt'), await settings.ariaSnapshot());
  await page.keyboard.press('Escape');
  console.log('PASS native settings dialog and CensorGuard policy/status');

  await page.getByRole('textbox', { name: '选择工作区', exact: true }).click();
  const picker = page.getByRole('dialog', { name: '选择工作区目录' });
  await picker.getByRole('button', { name: '编辑路径', exact: true }).click();
  const path = picker.getByRole('textbox', { name: '编辑路径' });
  await path.fill(workspace);
  await path.press('Enter');
  await picker.getByRole('button', { name: '打开', exact: true }).click();
  const input = page.locator('[data-composer-input][contenteditable="true"]').first();
  await input.waitFor();
  await input.fill('CensorGuard Web smoke test');
  await input.press('Enter');
  await page.getByText('CensorGuard Web test reply.', { exact: true }).waitFor({ timeout: 30000 });
  assert(modelRequests > 0, 'conversation must reach the local model stub');
  console.log('PASS workspace picker, conversation submission and streamed reply');
  writeFileSync(join(directory, 'conversation.txt'), await page.locator('body').ariaSnapshot());
  await page.getByRole('tab', { name: '轨迹', exact: true }).click();
  await page.getByText('CensorGuard Web smoke test', { exact: true }).first().waitFor();
  await page.getByRole('tab', { name: '安全拦截审计', exact: true }).click();
  await page.locator('[data-censorguard-audit][data-stream-state="live"]').waitFor();
  await page.getByRole('tab', { name: '对话', exact: true }).click();
  await page.getByText('CensorGuard Web test reply.', { exact: true }).waitFor();
  await page.getByRole('tab', { name: '安全拦截审计', exact: true }).click();
  await page.locator('[data-censorguard-audit][data-stream-state="live"]').waitFor();
  console.log('PASS conversation/trajectory/audit tab switching and audit re-entry');
  await page.getByRole('button', { name: '打开右侧边栏', exact: true }).click();
  const files = page.locator('[data-files-state="tree"]');
  await files.waitFor();
  await files.getByRole('button', { name: 'public.txt', exact: true }).click();
  await page.getByText('public test content', { exact: false }).waitFor();
  const filesTab = page.locator('[data-dockkit-tab]').filter({ has: page.getByText('文件', { exact: true }) });
  await filesTab.click();
  let deniedRequest;
  page.on('request', request => {
    if (request.url().includes('/api/workspaceFiles/read') && request.postData()?.includes('guarded.txt')) {
      deniedRequest = { url: request.url(), body: request.postDataJSON() };
    }
  });
  await files.getByRole('button', { name: 'guarded.txt', exact: true }).click();
  await waitFor(() => deniedRequest, 'protected preview did not issue a read');
  const audit = page.locator('[data-censorguard-audit]');
  await audit.locator('[data-audit-result="DENY"]').filter({ hasText: 'guarded.txt' }).first().waitFor();
  console.log('PASS sidebar file preview and actual BPF denial displayed in audit');
  const oldBoot = ctl('status').daemon_boot_id;
  run('censorguardd', ['reload', '--config', config]);
  const newBoot = ctl('status').daemon_boot_id;
  assert.notEqual(oldBoot, newBoot);
  await waitFor(() => (ctl('status').roots ?? []).includes(web.child.pid), 'DSH did not automatically reattach after reload');
  // Real-time streams have no replay: generate the probe after the Host has resubscribed.
  await waitFor(() => ctl('status').event_subscribers > 0, 'DSH audit did not resubscribe after reload');
  await page.request.post(deniedRequest.url, { data: deniedRequest.body });
  await waitFor(async () => (await audit.getAttribute('data-daemon-boot-id')) === newBoot, 'audit did not recover after daemon reload');
  await audit.locator('[data-audit-result="DENY"]').filter({ hasText: 'guarded.txt' }).first().waitFor();
  console.log('PASS daemon reload, automatic DSH reattachment and new-boot audit events');
  writeFileSync(join(directory, 'audit.txt'), await page.locator('body').ariaSnapshot());
  await page.screenshot({ path: join(directory, 'audit.png'), fullPage: true });
  assert.deepEqual(errors, []);
  console.log('PASS no browser errors; DSH Web acceptance passed');
} catch (error) {
  if (browser) for (const context of browser.contexts()) for (const page of context.pages()) {
    writeFileSync(join(directory, 'failure.txt'), await page.locator('body').ariaSnapshot().catch(() => 'page unavailable'));
    await page.screenshot({ path: join(directory, 'failure.png'), fullPage: true }).catch(() => {});
  }
  throw error;
} finally {
  await browser?.close();
  for (const child of children.reverse()) {
    if (child.exitCode !== null || child.signalCode !== null) continue;
    process.kill(-child.pid, 'SIGTERM');
    await Promise.race([new Promise(resolveExit => child.once('exit', resolveExit)), sleep(10000)]);
    if (child.exitCode === null && child.signalCode === null) process.kill(-child.pid, 'SIGKILL');
  }
  model.closeAllConnections();
  await new Promise(resolveClose => model.close(resolveClose));
}
