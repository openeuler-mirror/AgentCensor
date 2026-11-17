// Browser acceptance of the shipped wrapper with real Rust/ONNX and a local echo provider.
// Usage: PLAYWRIGHT_BROWSERS_PATH=... node scripts/test/dsh-web.mjs BUNDLE DSH_ROOT
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { performance } from 'node:perf_hooks';
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
const streaming = [];
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
  assert.equal(payload.stream, true);
  response.writeHead(200, { 'content-type': 'text/event-stream', connection: 'close' });
  const event = (delta, finish_reason = null) => response.write('data: ' + JSON.stringify({ choices: [{ index: 0, delta, finish_reason }] }) + '\n\n');
  event({ role: 'assistant', content: '' });
  if (message.tool_calls) {
    const call = message.tool_calls[0];
    event({ tool_calls: [{ index: 0, id: call.id, type: 'function', function: { name: call.function.name, arguments: '' } }] });
    for (const fragment of call.function.arguments.match(/[\s\S]{1,7}/gu)) event({ tool_calls: [{ index: 0, function: { arguments: fragment } }] });
  } else {
    const fragments = message.content.match(/[\s\S]{1,11}/gu) ?? [];
    const boundary = Math.min(32, fragments.length);
    for (const fragment of fragments.slice(0, boundary)) event({ content: fragment });
    const timing = { started: performance.now(), ended: false }; streaming.push(timing);
    if (!recovery) await delay(2500);
    for (const fragment of fragments.slice(boundary)) event({ content: fragment });
    timing.ended = performance.now();
  }
  event({}, message.tool_calls ? 'tool_calls' : 'stop');
  response.end('data: [DONE]\n\n');
});
await new Promise(ok => provider.listen(0, '127.0.0.1', ok));
const reserve = createServer(); await new Promise(ok => reserve.listen(0, '127.0.0.1', ok));
const port = reserve.address().port; await new Promise(ok => reserve.close(ok));
let web, browser, page, logs = '', succeeded = false;
try {
  await call('init', '--upstream', `http://127.0.0.1:${provider.address().port}`, '--model-id', 'deepseek-flash', '--no-key', '--port', String(port), '--ner-threads', '4');
  await call('start');
  const sample = (await readFile(join(bundle, existsSync(join(bundle, 'Cargo.toml'))
    ? 'examples/privacy-business.txt' : 'share/censorprivacy/examples/privacy-business.txt'), 'utf8')).trim();
  const cli = spawn(ctl, ['dsh', 'run', sample, '--dsh-root', dshRoot, '--config-dir', config], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  let cliText = '', cliErrors = '', cliEarly;
  cli.stdout.on('data', chunk => {
    cliText += chunk;
    if (cliText.includes('主题') && streaming.at(-1)?.ended === false) cliEarly ??= performance.now();
  });
  cli.stderr.on('data', chunk => { cliErrors += chunk; });
  const cliExit = await new Promise((ok, no) => { cli.once('error', no); cli.once('close', ok); });
  assert.equal(cliExit, 0, cliErrors);
  assert.ok(cliEarly, 'real gateway/DSH CLI must display text before upstream completion');
  assert.equal(cliText.match(/主题：/g)?.length, 1, 'CLI summary must not duplicate live output');
  assert.ok(cliText.includes('13800138000') && cliText.includes('[REDACTED_SECRET]'));
  const cliTiming = { first_text_ms: cliEarly - streaming[0].started, before_end_ms: streaming[0].ended - cliEarly };
  await assert.rejects(call('dsh', 'web', '--port', '-1'));
  web = spawn(ctl, ['dsh', 'web', '--config-dir', config, '--dsh-path', dshRoot, '--port', '0'], { env });
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
  const privacy = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 1000 } });
  privacy.on('pageerror', error => pageErrors.push(String(error)));
  const privacyURL = `http://127.0.0.1:${port}`;
  assert.ok((await call('web')).stdout.includes(privacyURL));
  await privacy.goto(privacyURL);
  await privacy.locator('.session-button').filter({ hasText: 'DeepSeek Harness' }).waitFor();
  const cliSessions = (await (await fetch(privacyURL + '/api/conversations')).json()).sessions;
  assert.equal(cliSessions.length, 1, 'CLI session must be automatically recorded');
  assert.equal(cliSessions[0].agent, 'deepseek-harness');
  await privacy.locator('.chat-message.role-user').filter({ hasText: '主题：联合研究项目交付与付款安排' }).locator('.record-button').first().click();
  assert.equal(await privacy.locator('#text-input').inputValue(), sample);
  assert.equal(await privacy.locator('#masked-text').inputValue(), requests[0].messages.find(m => m.role === 'user').content);
  await page.bringToFront();
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
  await input.fill(sample); await input.press('Enter');
  const answer = page.locator('[data-chat-flow-kind="assistant-step"]').last();
  await answer.filter({ hasText: '主题' }).waitFor({ timeout: 120000 });
  assert.equal(streaming.at(-1).ended, false, 'Web must render text before upstream completion');
  const webFirst = performance.now() - streaming.at(-1).started;
  await answer.filter({ hasText: '[REDACTED_SECRET]' }).waitFor({ timeout: 120000 });
  await page.waitForFunction(() => document.querySelector('[data-composer-input]')?.getAttribute('contenteditable') === 'true');
  // End-of-stream may precede the final UI projection by a short frame.
  await page.waitForFunction(() => [...document.querySelectorAll('[data-chat-flow-kind="assistant-step"]')].at(-1)?.textContent.includes('不要补写未提供的结论'));
  const displayed = await answer.innerText();
  assert.ok(displayed.includes('[REDACTED_SECRET]'), 'redacted reply missing');
  const values = ['张三', '李四', '清华大学', '北京大学', '13800138000', '13900139000',
    'zhangsan@example.com', 'lisi@example.org', '北京市海淀区中关村东路1号', '2026年11月5日', '12800.00', '3200.00'];
  assert.ok(requests.length > 0);
  const wire = JSON.stringify(requests);
  for (const value of [...values, 'DEMO_SECRET_DO_NOT_SEND']) assert.ok(!wire.includes(value), 'raw privacy reached upstream: ' + value);
  for (const value of values) assert.equal(displayed.split(value).length, sample.split(value).length, 'restoration changed repeated business data: ' + value);
  assert.ok(!displayed.includes('DEMO_SECRET_DO_NOT_SEND'));
  assert.ok(requests.every(payload => payload.stream === true && payload.thinking?.type === 'disabled'));
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
  await privacy.bringToFront();
  await privacy.waitForFunction(() => document.querySelectorAll('.session-button').length === 2);
  const sessions = (await (await fetch(privacyURL + '/api/conversations')).json()).sessions;
  const webSession = sessions.find(session => session.id !== cliSessions[0].id);
  assert.equal(webSession.agent, 'deepseek-harness');
  assert.ok(webSession.exchange_count >= 4, 'Web tool rounds must remain in the same session');
  await privacy.locator('.session-button').nth(sessions.findIndex(session => session.id === webSession.id)).click();
  await privacy.getByText('工具结果', { exact: true }).first().waitFor();
  await privacy.getByText('DeepSeek Harness 回复', { exact: true }).first().waitFor();
  assert.equal(await privacy.getByText('xiaoO 回复', { exact: true }).count(), 0);
  const detail = await (await fetch(privacyURL + '/api/conversations/' + webSession.id)).json();
  const userSnapshot = detail.exchanges[0].snapshots.find(snapshot => snapshot.text === sample);
  assert.equal(userSnapshot.analysis.masked_text, requests[1].messages.find(m => m.role === 'user').content);
  const token = (await readFile(join(config, 'gateway.token'), 'utf8')).trim();
  for (const path of ['/agents/custom-agent/manual-session/v1/chat/completions',
    '/xiaoo/0123456789abcdef0123456789abcdef/v1/chat/completions']) {
    const response = await fetch(privacyURL + path, { method: 'POST', headers: {
      authorization: `Bearer ${token}`, 'content-type': 'application/json',
    }, body: JSON.stringify({ model: 'deepseek-flash', stream: true, messages: [{ role: 'user', content: sample }] }) });
    assert.equal(response.status, 200);
    await response.text();
  }
  await privacy.waitForFunction(() => document.querySelectorAll('.session-button').length === 4);
  await privacy.locator('.session-button').filter({ hasText: 'custom-agent' }).click();
  await privacy.getByText('custom-agent 回复', { exact: true }).waitFor();
  await privacy.locator('.record-button').last().click();
  assert.equal(await privacy.locator('#text-input').inputValue(), sample);
  const artifacts = resolve(process.env.CP_DSH_WEB_SCREENSHOT_DIR || '.artifacts/agent-conversations');
  await mkdir(artifacts, { recursive: true });
  await privacy.screenshot({ path: join(artifacts, 'mixed-agents-desktop.png'), fullPage: true });
  await privacy.setViewportSize({ width: 390, height: 844 });
  assert.ok(await privacy.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await privacy.screenshot({ path: join(artifacts, 'mixed-agents-mobile.png'), fullPage: true });
  await privacy.locator('.session-button').filter({ hasText: 'xiaoO' }).click();
  await privacy.getByText('xiaoO 回复', { exact: true }).waitFor();
  await privacy.reload();
  await privacy.locator('.session-button').filter({ hasText: 'custom-agent' }).waitFor();
  assert.equal((await (await fetch(privacyURL + '/api/conversations')).json()).sessions.length, 4);
  assert.deepEqual(pageErrors, []);
  web.kill('SIGTERM');
  await new Promise(ok => web.once('close', ok));
  assert.equal((await call('check')).stderr, '', 'closing Web must leave the gateway running');
  await assert.rejects(fetch(new URL('/', startupURL)), 'closing Web must close its listener');
  succeeded = true;
  if (process.env.CP_DSH_WEB_REPORT) await writeFile(process.env.CP_DSH_WEB_REPORT, JSON.stringify({ passed: true, cli: cliTiming, web_first_text_ms: webFirst, upstream_requests: requests.length, real_ner_threads: 4, split_reference_and_secret: true, protected_tool_recovery: true,
    privacy_workspace: ['automatic_cli_session', 'automatic_web_session', 'tool_round_grouping', 'actual_masking_details', 'generic_base_url', 'mixed_agent_labels', 'legacy_xiaoo', 'refresh_persistence', 'mobile_layout'] }, null, 2) + '\n');
  console.log('PASS Harness CLI/Web and privacy workspace: automatic sessions, exact masking details, tools, generic Base URL, mixed Agent sources, legacy xiaoO, refresh and mobile.');
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
