// Actual xiaoO + one Rust/ONNX gateway + the rendered conversation workspace.
import assert from 'node:assert/strict';
import { createServer, request } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, readFile, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { createRequire } from 'node:module';

process.umask(0o077);
const exec = promisify(execFile);
const root = resolve(import.meta.dirname, '../..');
const bundle = process.env.CP_TEST_BUNDLE ? resolve(process.env.CP_TEST_BUNDLE) : null;
const ctl = join(bundle || root, 'cpctl');
const binary = resolve(process.argv[2] || 'target/debug/censorprivacy');
const xiaoo = resolve(process.argv[3] || '/root/xiaoO/target/debug/xiaoo');
const { chromium } = createRequire(import.meta.url)(process.env.CP_PLAYWRIGHT_MODULE || 'playwright');
const directory = await mkdtemp(join(tmpdir(), 'cp-xiaoo-web-'));
const config = join(directory, 'control');
const prefix = bundle || join(directory, 'prefix');
const home = join(directory, 'home');
await mkdir(home);
if (!bundle) {
  await mkdir(join(prefix, 'bin'), {recursive:true});
  await symlink(binary, join(prefix, 'bin/censorprivacy'));
}
const env = {...(bundle ? {PATH:'/usr/bin:/bin',LANG:'C.UTF-8'} : process.env),
  HOME:home, XDG_CONFIG_HOME:join(home,'.config'), CP_INSTALL_PREFIX:prefix};
const call = (...args) => {
  const pending = exec(ctl, [...args, '--config-dir', config, '--prefix', prefix], {env, timeout:180000,maxBuffer:4*1024*1024});
  pending.child.stdin.end();
  return pending;
};
const captured = [];
const provider = createServer(async (request, response) => {
  try {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const payload = JSON.parse(Buffer.concat(chunks));
    assert.equal(request.url, '/v1/chat/completions');
    captured.push(payload);
    const tool = payload.messages.findLast(message => message.role === 'tool');
    const demo = payload.messages.some(message => message.role === 'user' && message.content?.includes('file_read'));
    const message = demo && !tool ? {tool_calls:[{id:'read_mail',type:'function',function:{name:'file_read',arguments:'{"file_path":"privacy-business.txt"}'}}]}
      : {content:tool ? JSON.parse(tool.content).content : payload.messages.findLast(message => message.role === 'user').content};
    response.writeHead(200, {'content-type':'text/event-stream',connection:'close'});
    const event = (delta, finish = null) => response.write('data: ' + JSON.stringify({id:'fixture',created:0,object:'chat.completion.chunk',model:payload.model,
      choices:[{index:0,delta,finish_reason:finish}]}) + '\n\n');
    event({role:'assistant',content:''});
    if (message.tool_calls) {
      const tool = message.tool_calls[0];
      event({tool_calls:[{index:0,id:tool.id,type:'function',function:{name:tool.function.name,arguments:''}}]});
      for (const part of tool.function.arguments.match(/.{1,7}/gu)) event({tool_calls:[{index:0,function:{arguments:part}}]});
    } else {
      for (const part of message.content.match(/[\s\S]{1,11}/gu) || []) event({content:part});
    }
    event({},message.tool_calls ? 'tool_calls' : 'stop');
    response.end('data: [DONE]\n\n');
  } catch (error) {response.writeHead(500);response.end();console.error(error);}
});
let browser;
let succeeded = false;
try {
  await new Promise(ok => provider.listen(0,'127.0.0.1',ok));
  const reserve = createServer();await new Promise(ok => reserve.listen(0,'127.0.0.1',ok));
  const port = reserve.address().port;await new Promise(ok => reserve.close(ok));
  const origin = `http://127.0.0.1:${port}`;
  await call('init','--upstream',`http://127.0.0.1:${provider.address().port}`,'--model-id','deepseek-flash','--no-key','--port',String(port),'--ner-threads','4');
  await call('start');
  console.log('Gateway ready; running actual xiaoO SSE task.');
  assert.ok((await call('xiaoo','web')).stdout.includes(origin));
  browser = await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE,args:['--no-sandbox','--disable-dev-shm-usage']});
  const page = await browser.newPage({viewport:{width:1440,height:1000}});
  const errors = [];page.on('pageerror',error => errors.push(error.message));
  await page.goto(origin);
  await page.getByText('尚无会话',{exact:true}).waitFor();
  assert.equal(await page.locator('#text-input').inputValue(),'');
  const text = '😀请复述：姓名：张三；邮箱：alice@example.com；手机：13800138000；密码：Demo!2026';
  const agent = await call('xiaoo','run',text,'--xiaoo-bin',xiaoo);
  assert.ok(agent.stdout.includes('alice@example.com'));
  assert.ok(agent.stdout.includes('[REDACTED_SECRET]'));
  assert.ok(!agent.stdout.includes('Demo!2026'));
  assert.equal(captured.at(-1).tools, undefined, 'default CLI must omit tools');
  await page.locator('.session-button').waitFor({timeout:10000});
  await page.locator('.record-button').filter({hasText:'查看检测与遮蔽'}).last().click();
  assert.equal(await page.locator('#text-input').inputValue(),text);
  assert.ok(await page.locator('#text-input').evaluate(element => element.readOnly));
  const sessions = await (await fetch(origin+'/api/conversations')).json();
  const detail = await (await fetch(origin+'/api/conversations/'+sessions.sessions[0].id)).json();
  assert.equal(detail.exchanges[0].status,'completed');
  const snapshot = detail.exchanges[0].snapshots.find(snapshot => snapshot.text === text);
  assert.equal(await page.locator('#masked-text').inputValue(),captured[0].messages.findLast(m => m.role === 'user').content);
  assert.equal(snapshot.analysis.masked_text,await page.locator('#masked-text').inputValue());
  assert.equal(await page.locator('#highlighted-text').textContent(),text);
  await page.locator('.finding').filter({has:page.locator('.finding-snippet',{hasText:'alice@example.com'})}).click();
  assert.equal(await page.locator('mark.active').textContent(),'alice@example.com');
  assert.ok(await page.locator('#analyze').isDisabled());
  // A refresh must retain the persisted history and its original CP references.
  const masked = await page.locator('#masked-text').inputValue();
  await page.reload();
  await page.locator('.record-button').filter({hasText:'查看检测与遮蔽'}).last().click();
  assert.equal(await page.locator('#masked-text').inputValue(),masked);
  await call('xiaoo','demo','--enable-tools');
  console.log('Actual file_read completed; checking rendered tool history.');
  await page.waitForFunction(() => document.querySelectorAll('.session-button').length === 2);
  await page.locator('.session-button').first().click();
  await page.getByText('工具结果',{exact:true}).waitFor({timeout:15000});
  const current = await (await fetch(origin+'/api/conversations')).json();
  const toolSession = current.sessions.find(session => session.id !== sessions.sessions[0].id);
  const toolRecords = await (await fetch(origin+'/api/conversations/'+toolSession.id)).json();
  assert.ok(toolRecords.exchanges.some(exchange => exchange.response.tool_calls?.[0]?.function.name === 'file_read'));
  const toolExchange = toolRecords.exchanges.find(exchange => exchange.messages.some(m => m.role === 'tool'));
  assert.ok(toolExchange);
  const toolSnapshot = toolExchange.snapshots.find(s => /\/messages\/\d+\/content/.test(s.path) && s.text.includes('DEMO_SECRET_DO_NOT_SEND'));
  assert.ok(toolSnapshot,'must capture the tool payload before masking');
  assert.ok(!toolSnapshot.analysis.masked_text.includes('DEMO_SECRET_DO_NOT_SEND'));
  assert.ok(captured.every(payload => !JSON.stringify(payload).includes('DEMO_SECRET_DO_NOT_SEND')));
  const toolMessage = page.locator('.chat-message.role-tool');
  await toolMessage.locator('.record-button').first().click();
  assert.ok((await page.locator('#text-input').inputValue()).includes('DEMO_SECRET_DO_NOT_SEND'));
  const artifacts = resolve(process.env.CP_WEB_SCREENSHOT_DIR || '.artifacts/xiaoo-web');
  await mkdir(artifacts,{recursive:true});
  await page.screenshot({path:join(artifacts,'desktop.png'),fullPage:true});
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.screenshot({path:join(artifacts,'mobile.png'),fullPage:true});
  await page.locator('#manual-mode').click();
  await page.locator('#text-input').fill('邮箱：bob@example.com');
  await page.locator('#analyze').click();
  await page.locator('#result-content').waitFor();
  assert.ok(!(await page.locator('#masked-text').inputValue()).includes('bob@example.com'));
  const foreign = await new Promise((ok,no) => {
    const req = request(origin+'/api/conversations',{headers:{host:'foreign.example'}},response => {response.resume();ok(response.statusCode);});
    req.on('error',no);req.end();
  });
  assert.equal(foreign,403);
  assert.equal((await fetch(origin+'/api/conversations',{headers:{origin:'https://foreign.example'}})).status,403);
  assert.deepEqual(errors,[]);
  await browser.close();browser=null;
  console.log('Browser checks passed; running the native TUI entry in a PTY.');
  const tui = await exec('python3',[join(root,'scripts/test/xiaoo-tui.py'),ctl,config,prefix,
    dirname(dirname(dirname(xiaoo))),origin,join(artifacts,'tui-terminal.log')],{env,timeout:200000,maxBuffer:1024*1024});
  assert.equal(JSON.parse(tui.stdout).passed,true);
  assert.equal(captured.at(-1).tools, undefined, 'default native TUI must omit tools upstream');
  const beforeEnabledTui = captured.length;
  const enabledTui = await exec('python3',[join(root,'scripts/test/xiaoo-tui.py'),ctl,config,prefix,
    dirname(dirname(dirname(xiaoo))),origin,join(artifacts,'tui-enabled-terminal.log'),'--enable-tools'],
    {env,timeout:200000,maxBuffer:1024*1024});
  assert.equal(JSON.parse(enabledTui.stdout).passed,true);
  assert.equal(captured.length,beforeEnabledTui+1,'TUI preparation must stay local');
  assert.ok(captured.at(-1).tools.length>1,'enabled TUI must retain its native tools');
  await call('stop');await call('start');
  assert.equal((await (await fetch(origin+'/api/conversations')).json()).sessions.length,4);
  const persisted = await (await fetch(origin+'/api/conversations/'+sessions.sessions[0].id)).json();
  assert.equal(persisted.exchanges[0].snapshots.find(snapshot => snapshot.text === text).analysis.masked_text,masked);

  console.log('Checking confirmed reset, stale polling, other tabs and persistence.');
  browser = await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE,args:['--no-sandbox','--disable-dev-shm-usage']});
  const resetPage = await browser.newPage({viewport:{width:1440,height:1000}});
  resetPage.on('pageerror', error => errors.push(error.message));
  await resetPage.goto(origin);
  await resetPage.locator('.record-button').filter({hasText:'查看检测与遮蔽'}).last().click();
  const observer = await browser.newPage();
  await observer.goto(origin);
  await observer.locator('.record-button').filter({hasText:'查看检测与遮蔽'}).last().click();
  await resetPage.bringToFront();
  resetPage.once('dialog', dialog => dialog.dismiss());
  await resetPage.locator('#clear-conversations').click();
  assert.equal((await (await fetch(origin+'/api/conversations')).json()).sessions.length,4);
  assert.ok(await resetPage.locator('#text-input').evaluate(element => element.readOnly));

  // Hold a pre-reset poll in transit. Its late result must not repopulate UI.
  const staleList = await (await fetch(origin+'/api/conversations')).json();
  let unblockPoll, pollStarted;
  const blockedPoll = new Promise(ok => {unblockPoll = ok;});
  const sawPoll = new Promise(ok => {pollStarted = ok;});
  let holdPoll = true;
  await resetPage.route('**/api/conversations', async route => {
    if (route.request().method() === 'GET' && holdPoll) {
      holdPoll = false;pollStarted();await blockedPoll;
      await route.fulfill({json:staleList});
    } else await route.continue();
  });
  await sawPoll;
  resetPage.once('dialog', dialog => dialog.accept());
  await resetPage.locator('#clear-conversations').click();
  await resetPage.getByText('尚无会话',{exact:true}).waitFor();
  unblockPoll();
  await resetPage.waitForFunction(() => document.querySelector('#clear-conversations').disabled);
  assert.equal(await resetPage.locator('.session-button').count(),0);
  assert.equal(await resetPage.locator('.exchange').count(),0);
  assert.equal(await resetPage.locator('#text-input').inputValue(),'');
  assert.equal(await resetPage.locator('#masked-text').inputValue(),'');
  assert.ok(!(await resetPage.locator('#text-input').evaluate(element => element.readOnly)));
  assert.equal((await fetch(origin+'/api/conversations/'+sessions.sessions[0].id)).status,404);
  await observer.bringToFront();
  await observer.getByText('尚无会话',{exact:true}).waitFor();
  await observer.waitForFunction(() => document.querySelector('#text-input').value === '');
  assert.equal(await observer.locator('.exchange').count(),0);
  assert.equal(await observer.locator('#masked-text').inputValue(),'');
  await observer.close();
  await resetPage.unroute('**/api/conversations');
  await resetPage.reload();
  await resetPage.getByText('尚无会话',{exact:true}).waitFor();
  assert.equal(await resetPage.locator('#text-input').inputValue(),'');
  assert.equal(await resetPage.locator('#masked-text').inputValue(),'');
  await resetPage.screenshot({path:join(artifacts,'reset-empty.png'),fullPage:true});
  await resetPage.setViewportSize({width:390,height:844});
  assert.ok(await resetPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await resetPage.screenshot({path:join(artifacts,'reset-empty-mobile.png'),fullPage:true});
  await call('xiaoo','run','这是清空后的全新测试。','--xiaoo-bin',xiaoo);
  await resetPage.locator('.session-button').waitFor();
  const fresh = await (await fetch(origin+'/api/conversations')).json();
  assert.equal(fresh.sessions.length,1);
  assert.ok(!staleList.sessions.some(session => session.id === fresh.sessions[0].id));
  assert.ok(fresh.sessions[0].title.includes('这是清空后的全新测试'));
  const reset = await fetch(origin+'/api/conversations',{method:'DELETE',headers:{'content-type':'application/json'},body:JSON.stringify({confirm:true})});
  assert.equal(reset.status,200);
  assert.equal((await reset.json()).cleared_sessions,1);
  await browser.close();browser=null;
  await call('stop');await call('start');
  assert.deepEqual((await (await fetch(origin+'/api/conversations')).json()).sessions,[]);
  assert.deepEqual(errors,[]);
  await writeFile(join(artifacts,'report.json'),JSON.stringify({passed:true,timestamp:new Date().toISOString(),bundle,actual_xiaoo:xiaoo,upstream_requests:captured.length,
    tui_default:JSON.parse(tui.stdout),tui_enabled:JSON.parse(enabledTui.stdout),
    checks:['native_xiaoo_tui','default_tui_tools_disabled','enabled_tui_startup_preparation_local','native_xiaoo_sse','actual_request_masking','response_restoration','tool_call_and_result','session_isolation','click_details','unicode_highlights','refresh','gateway_restart','manual_detection','mobile_layout','local_boundary','clear_confirmation','clear_stale_poll','clear_other_tabs','clear_details','clear_new_session','clear_restart_persistence']},null,2)+'\n');
  console.log('PASS actual xiaoO + shared ONNX gateway + conversation Web, persisted exact masking, SSE, tools, clicks, restart and mobile.');
  succeeded=true;
} finally {
  await browser?.close();
  await call('stop').catch(()=>{});
  provider.closeAllConnections();await new Promise(ok => provider.close(ok));
  if (succeeded) await rm(directory,{recursive:true,force:true});
  else console.log('Private diagnostics: '+directory);
}
