// Real Rust/ONNX + browser acceptance. No Python server or external model request.
// CP_PLAYWRIGHT_MODULE may name an absolute local Playwright module directory.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { request } from 'node:http';
import { resolve, join } from 'node:path';
import { createInterface } from 'node:readline';

const { chromium } = createRequire(import.meta.url)(process.env.CP_PLAYWRIGHT_MODULE || 'playwright');
const server = spawn(resolve(process.argv[2] || 'target/debug/censorprivacy'), ['web', '--port', '0'], {
  env: process.env, stdio: ['ignore', 'pipe', 'pipe'],
});
let diagnostics = '';
server.stderr.on('data', chunk => { diagnostics += chunk; });
let browser;
try {
  const origin = await new Promise((ok, fail) => {
    const timer = setTimeout(() => fail(new Error('Web startup timed out: ' + diagnostics)), 60000);
    server.once('error', error => { clearTimeout(timer); fail(error); });
    server.once('exit', () => { clearTimeout(timer); fail(new Error('Web exited: ' + diagnostics)); });
    createInterface({ input: server.stdout }).on('line', line => {
      const match = line.match(/^Privacy Detector: (http:\/\/127\.0\.0\.1:\d+)\/$/);
      if (match) { clearTimeout(timer); ok(match[1]); }
    });
  });
  const analyze = payload => fetch(origin + '/api/analyze', {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(payload),
  });
  for (const payload of [{text:''}, {text:'x'.repeat(5001)}, {text:'x',mask_threshold:null},
    {text:'x',mask_threshold:2}, {text:'x',mask_threshold:'0.6'}, {text:'x',use_ner:'false'},
    {text:'x',mask_style:'unknown'}, {text:'x',unknown:true}]) {
    assert.equal((await analyze(payload)).status, 400);
  }
  assert.equal((await analyze({text:'x'.repeat(70000)})).status, 413);
  const foreignHost = await new Promise((ok, fail) => {
    const probe = request(origin, {headers:{host:'foreign.example'}}, response => {
      response.resume(); ok(response.statusCode);
    });
    probe.on('error',fail); probe.end();
  });
  assert.equal(foreignHost, 403);
  assert.equal((await fetch(origin + '/api/analyze', {method:'POST', headers:{origin:'https://foreign.example','content-type':'application/json'},body:'{"text":"x"}'})).status, 403);

  browser = await chromium.launch({headless:true,
    ...(process.env.CHROMIUM_EXECUTABLE ? {executablePath:process.env.CHROMIUM_EXECUTABLE} : {}),
    args:['--no-sandbox','--disable-dev-shm-usage'],
  });
  const page = await browser.newPage({viewport:{width:1440,height:1000}});
  const errors = [];
  const requests = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => requests.push(request.url()));
  const response = await page.goto(origin);
  assert.equal(response.headers()['cache-control'], 'no-store');
  assert.equal(await page.title(), '隐私检测台 · Privacy Detector');
  const text = '😀张三在清华大学工作，邮箱alice@example.com。\n配送地址：广东省广州市荔湾区人民路68号27栋1442室，联系人张三（13800138000）。\n车牌号京AABCDE；护照E12345678；身份证号110105 19491231 002X；还款卡号4111111111111112；普通数量42\n密码："two words!"；重复密码：two words!';
  await page.locator('#text-input').fill(text);
  await page.locator('#mask-style').selectOption('label');
  await page.locator('#analyze').click();
  await page.locator('#result-content').waitFor({timeout:60000});
  assert.equal(await page.locator('#highlighted-text').textContent(), text);
  const snippets = await page.locator('.finding-snippet').allTextContents();
  for (const value of ['张三','清华大学','alice@example.com','广东省广州市荔湾区人民路68号27栋1442室',
    '京AABCDE','E12345678','110105 19491231 002X','4111111111111112','42']) assert.ok(snippets.includes(value), value);
  assert.ok(!snippets.includes('alice'), 'mailbox local part must not become a person');
  const plate = page.locator('.finding').filter({has:page.locator('.finding-snippet', {hasText:'京AABCDE'})});
  assert.ok((await plate.innerText()).includes('校验未通过'));
  await plate.click();
  assert.equal(await page.locator('mark.active').textContent(), '京AABCDE');
  const masked = await page.locator('#masked-text').inputValue();
  for (const value of ['张三','清华大学','alice@example.com','京AABCDE','E12345678','110105 19491231 002X','4111111111111112','two words!']) assert.ok(!masked.includes(value), value);
  assert.ok(masked.includes('普通数量42'));
  assert.ok(masked.includes('[REDACTED_SECRET]'));
  const directory = resolve(process.env.CP_WEB_SCREENSHOT_DIR || '.artifacts/detector-web');
  await mkdir(directory, {recursive:true});
  await page.screenshot({path:join(directory,'desktop.png'),fullPage:true});
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:join(directory,'mobile.png'),fullPage:true});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.locator('input[value="ner"]').uncheck();
  await page.locator('#text-input').fill('计划42个任务；IP：192.0.2.10；车牌：京A12345。');
  await page.locator('#mask-style').selectOption('stars');
  await page.locator('#mask-threshold').fill('0.4');
  await page.locator('#analyze').click();
  await page.locator('#result-content').waitFor();
  assert.equal(await page.locator('#masked-text').inputValue(), '计划**个任务；IP：**********；车牌：*******。');
  await page.locator('#mask-threshold').fill('2');
  assert.ok(await page.locator('#analyze').isDisabled());
  await page.locator('#clear').click();
  assert.equal(await page.locator('#text-input').inputValue(), '');
  assert.ok(await page.locator('#masked-panel').isHidden());
  assert.deepEqual(await page.evaluate(() => [localStorage.length,sessionStorage.length]),[0,0]);
  assert.ok(requests.every(url => new URL(url).origin === origin));
  assert.deepEqual(errors,[]);
  console.log('PASS detector Web: real ONNX, Unicode highlights, full address, invalid contextual fields, independent mask threshold, mobile layout, validation and local-only API.');
} finally {
  await browser?.close();
  if (server.exitCode === null && server.signalCode === null) {
    await new Promise(ok => {
      const timer = setTimeout(() => server.kill('SIGKILL'),5000);
      server.once('exit',() => {clearTimeout(timer);ok();});
      server.kill('SIGTERM');
    });
  }
}
