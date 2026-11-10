// Browser acceptance of the shipped wrapper with real Rust/ONNX and a local echo provider.
// Usage: PLAYWRIGHT_BROWSERS_PATH=... node scripts/test/dsh-web.mjs BUNDLE DSH_ROOT
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';

process.umask(0o077);
const exec = promisify(execFile);
const bundle = resolve(process.argv[2]);
const dshRoot = resolve(process.argv[3]);
const { chromium } = createRequire(join(dshRoot, 'apps/web/package.json'))('playwright');
const directory = await mkdtemp(join(tmpdir(), 'cp-web-test-'));
const config = join(directory, 'control');
const home = join(directory, 'home'); await mkdir(home, { mode: 0o700 });
const env = { PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: join(home, '.config'), LANG: 'C.UTF-8' };
const ctl = join(bundle, 'cpctl');
const call = (...args) => exec(ctl, [...args, '--config-dir', config], { env, timeout: 120000 });
const requests = [];
const provider = createServer(async (request, response) => {
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const payload = JSON.parse(Buffer.concat(chunks)); requests.push(payload);
  const mail = payload.messages.find(message => message.role === 'user' && message.content.includes('主题：联合研究项目交付与付款安排')).content;
  const recovery = payload.messages.some(message => message.role === 'user' && message.content.includes('工具回归'));
  const results = payload.messages.filter(message => message.role === 'tool');
  let message = { role: 'assistant', content: mail };
  if (recovery && results.length < 2) {
    const operation = results.length ? 'sum' : 'date_diff';
    const refs = results.length ? [...mail.matchAll(/<CP_MONEY_[0-9a-f]{32}>/g)].slice(0, 2).map(match => match[0])
      : [mail.match(/<CP_DATE_[0-9a-f]{32}>/)[0]];
    message = { role: 'assistant', content: null, tool_calls: [{ id: `recovery_${operation}`, type: 'function',
      function: { name: 'private_compute', arguments: JSON.stringify({ operation, refs }) } }] };
  } else if (recovery) {
    message.content = `工具回归完成；合计 ${JSON.parse(results.at(-1).content).results.sum}`;
  }
  response.writeHead(200, { 'content-type': 'application/json', connection: 'close' });
  response.end(JSON.stringify({ choices: [{ index: 0, message, finish_reason: message.tool_calls ? 'tool_calls' : 'stop' }] }));
});
await new Promise(ok => provider.listen(0, '127.0.0.1', ok));
const reserve = createServer(); await new Promise(ok => reserve.listen(0, '127.0.0.1', ok));
const port = reserve.address().port; await new Promise(ok => reserve.close(ok));
let web, browser, page, logs = '', succeeded = false;
try {
  await call('init', '--upstream', `http://127.0.0.1:${provider.address().port}`, '--model-id', 'deepseek-flash', '--no-key', '--port', String(port));
  await call('start');
  await assert.rejects(call('dsh', 'web', '--port', '-1'));
  web = spawn(ctl, ['dsh', 'web', '--config-dir', config, '--dsh-root', dshRoot, '--port', '0'], { env });
  web.stdout.on('data', chunk => { logs += chunk; });
  web.stderr.on('data', chunk => { logs += chunk; });
  let startupURL;
  for (let i = 0; i < 600; i++) {
    assert.equal(web.exitCode, null, logs.replace(/token=[^\s&]+/g, 'token=[private]'));
    startupURL = logs.match(/dsh web: (http:\/\/\S+)/)?.[1];
    if (startupURL) break;
    await delay(100);
  }
  assert.ok(startupURL, 'Web startup URL missing');
  assert.equal((await fetch(new URL('/', startupURL))).status, 401, 'unauthenticated Web must require a token');
  browser = await chromium.launch({ executablePath: process.env.CP_BROWSER_EXECUTABLE });
  page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 1000 } });
  page.setDefaultTimeout(30000);
  const pageErrors = []; page.on('pageerror', error => pageErrors.push(String(error)));
  await page.goto(startupURL);
  await page.getByRole('button', { name: '继续', exact: true }).click();
  await page.getByRole('textbox', { name: '选择工作区', exact: true }).click();
  const picker = page.getByRole('dialog', { name: '选择工作区目录' });
  await picker.getByRole('button', { name: '编辑路径', exact: true }).click();
  const path = picker.getByRole('textbox', { name: '编辑路径' });
  await path.fill(join(config, 'dsh-web/workspace')); await path.press('Enter');
  await picker.getByRole('button', { name: '打开', exact: true }).click();
  const input = page.locator('[data-composer-input][contenteditable="true"]').first();
  await input.waitFor();
  assert.ok((await page.locator('body').innerText()).includes('deepseek-flash'), 'configured model must be available without entering another key');
  const sample = (await readFile(join(bundle, 'share/censorprivacy/examples/privacy-business.txt'), 'utf8')).trim();
  await input.fill(sample); await input.press('Enter');
  const answer = page.locator('[data-chat-flow-kind="assistant-step"]').last();
  await answer.waitFor({ timeout: 120000 });
  const displayed = await answer.innerText();
  assert.ok(displayed.includes('[REDACTED_SECRET]'), 'redacted reply missing');
  const values = ['张三', '李四', '清华大学', '北京大学', '13800138000', '13900139000',
    'zhangsan@example.com', 'lisi@example.org', '北京市海淀区中关村东路1号', '2026年11月5日', '12800.00', '3200.00'];
  assert.ok(requests.length > 0);
  const wire = JSON.stringify(requests);
  for (const value of [...values, 'DEMO_SECRET_DO_NOT_SEND']) assert.ok(!wire.includes(value), 'raw privacy reached upstream: ' + value);
  for (const value of values) assert.equal(displayed.split(value).length, sample.split(value).length, 'restoration changed repeated business data: ' + value);
  assert.ok(!displayed.includes('DEMO_SECRET_DO_NOT_SEND'));
  assert.ok(requests.every(payload => payload.thinking?.type === 'disabled'));
  assert.ok(requests.every(payload => payload.tools?.every(tool => tool.function.name === 'private_compute')), 'private-only profile must not advertise subagent tools');
  await input.fill('工具回归：计算邮件中两笔服务费之和。日期差缺少第二个日期，不要猜测当前日期。'); await input.press('Enter');
  const recovered = page.locator('[data-chat-flow-kind="assistant-step"]').filter({ hasText: '工具回归完成' }).last();
  await recovered.waitFor({ timeout: 240000 });
  assert.ok((await recovered.innerText()).includes('16000.00'), 'valid sum after the invalid date call must restore');
  assert.ok(requests.some(payload => payload.messages.some(message => message.role === 'tool' && message.content.includes('DATE_DIFF_REQUIRES_TWO_DATE_REFERENCES'))), 'invalid date call must reach the model as a recoverable tool error');
  const finalWire = JSON.stringify(requests);
  for (const value of [...values, 'DEMO_SECRET_DO_NOT_SEND', '16000.00']) assert.ok(!finalWire.includes(value), 'raw privacy reached upstream during tool recovery: ' + value);
  assert.deepEqual(pageErrors, []);
  await page.screenshot({ path: join(directory, 'chat.png'), fullPage: true });
  web.kill('SIGTERM');
  await new Promise(ok => web.once('close', ok));
  assert.equal((await call('check')).stderr, '', 'closing Web must leave the gateway running');
  await assert.rejects(fetch(new URL('/', startupURL)), 'closing Web must close its listener');
  succeeded = true;
  console.log('PASS Harness Web: authenticated page, configured model, long-text masking/restoration, SECRET redaction, private-only tools, invalid-date recovery followed by protected sum, and independent shutdown.');
} finally {
  if (!succeeded && page) {
    await writeFile(join(directory, 'page.txt'), await page.locator('body').ariaSnapshot().catch(() => 'Page unavailable'));
    await page.screenshot({ path: join(directory, 'failure.png'), fullPage: true }).catch(() => {});
  }
  if (browser) await browser.close();
  if (web && web.exitCode === null && web.signalCode === null) {
    const stopped = new Promise(ok => web.once('close', ok)); web.kill('SIGTERM'); await stopped;
  }
  await call('stop').catch(() => {});
  provider.closeAllConnections(); await new Promise(ok => provider.close(ok));
  if (succeeded) await rm(directory, { recursive: true, force: true });
  else {
    await writeFile(join(directory, 'web.log'), logs);
    await writeFile(join(directory, 'mock-requests.json'), JSON.stringify(requests, null, 2));
    console.log('Private diagnostics: ' + directory);
  }
}
