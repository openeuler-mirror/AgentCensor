// Run as an ordinary user in a fresh, writable extraction parent directory.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, readFile, writeFile, rename, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

const exec = promisify(execFile);
const original = resolve(process.argv[2]);
const report = resolve(process.argv[3]);
const directory = await mkdtemp(join(tmpdir(), 'cp-relocation-'));
const home = join(directory, 'home');
const config = join(directory, 'control');
await mkdir(home, {mode:0o700});
const env = {PATH:'/usr/bin:/bin',HOME:home,XDG_CONFIG_HOME:join(home,'.config'),LANG:'C.UTF-8'};
const destination = join(dirname(original), 'relocated 迁移后的目录');
let bundle = original;
const checks = [];
const call = async (...args) => {
  const result = await exec(join(bundle,'censorprivacy'), [...args,'--config-dir',config],
    {env,cwd:directory,timeout:180000,maxBuffer:4*1024*1024});
  return result;
};
const requests = [];
const provider = createServer(async (request,response) => {
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = JSON.parse(Buffer.concat(chunks)); requests.push(body);
  response.writeHead(200, {'content-type':'application/json',connection:'close'});
  response.end(JSON.stringify({choices:[{index:0,message:{role:'assistant',content:body.messages.at(-1).content},finish_reason:'stop'}]}));
});
await new Promise(ok => provider.listen(0,'127.0.0.1',ok));
const reserve = createServer();await new Promise(ok => reserve.listen(0,'127.0.0.1',ok));
const port = reserve.address().port;await new Promise(ok => reserve.close(ok));
try {
  await call('init','--upstream',`http://127.0.0.1:${provider.address().port}`,
    '--model-id','relocation-model','--no-key','--port',String(port));
  await writeFile(join(config,'install.json'),JSON.stringify({bin_dir:join(original,'bin'),
    native_library:join(original,'lib/censorprivacy/libcensorprivacy_tls_guard.so')}),{mode:0o600});
  const policy = await readFile(join(bundle,'share/censorprivacy/config.yaml'));
  const token = await readFile(join(config,'gateway.token'),'utf8');
  await call('start');await call('check');await call('stop');
  await rename(original,destination);bundle=destination;
  await call('start');await call('check');
  const state = JSON.parse(await readFile(join(config,'process.json')));
  const maps = await readFile(`/proc/${state.pid}/maps`,'utf8');
  assert.ok(maps.includes(join(destination,'lib/censorprivacy/libonnxruntime.so')));
  const response = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`,{
    method:'POST',headers:{authorization:'Bearer '+token.trim(),'content-type':'application/json'},
    body:JSON.stringify({model:'relocation-model',messages:[{role:'user',content:'姓名：张三，邮箱alice@example.com。'}]}),
    signal:AbortSignal.timeout(90000),
  });
  const completion = await response.json();assert.equal(response.status,200,JSON.stringify(completion.error));
  assert.ok(completion.choices[0].message.content.includes('alice@example.com'));
  assert.ok(!JSON.stringify(requests.at(-1)).includes('alice@example.com'));
  assert.ok(!JSON.stringify(requests.at(-1)).includes('张三'));
  assert.deepEqual(await readFile(join(bundle,'share/censorprivacy/config.yaml')),policy);
  assert.equal(await readFile(join(config,'gateway.token'),'utf8'),token);
  checks.push('move_after_init_preserves_config_and_credentials','spaces_and_chinese_path',
    'ignores_stale_install_path','loads_onnx_from_new_directory','real_detection_and_restoration_after_move');
  await call('stop');
  const selfTest = await exec(join(bundle,'censorprivacy'),['self-test'],{env,cwd:directory,timeout:180000,maxBuffer:4*1024*1024});
  assert.match(selfTest.stdout,/完整交付包自检/);
  checks.push('complete_rootless_gateway_and_tls_self_test_after_move');
  await writeFile(report,JSON.stringify({passed:true,timestamp:new Date().toISOString(),uid:process.getuid(),
    original,destination,checks},null,2)+'\n');
  console.log('PASS relocation after init with unchanged credentials, Chinese/spaced path, bundled runtime and TLS.');
} finally {
  await call('stop').catch(()=>{});
  if(bundle!==original) await rename(destination,original);
  provider.closeAllConnections();await new Promise(ok=>provider.close(ok));
  await rm(directory,{recursive:true,force:true});
}
