// No ONNX or cloud: exercise actual shell selection and fast-exit handling.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const exec = promisify(execFile);
const root = resolve(import.meta.dirname, '..');
const common = join(root, 'scripts/ops/common.sh');
const shell = (code, args = []) => exec('bash', ['-c', 'source "$1"; shift; ' + code, 'ctl-test', common, ...args]);
async function binary(directory, content = '#!/usr/bin/env bash\nexit 64\n') {
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, 'censorprivacy'), content, { mode: 0o755 });
}

test('entry location selects local source/installed binaries ahead of stale install records', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-select-'));
  const source = join(directory, 'source'), installed = join(directory, 'installed');
  const legacy = join(directory, 'legacy'), explicit = join(directory, 'explicit'), config = join(directory, 'config');
  try {
    await mkdir(source); await mkdir(config, { mode: 0o700 });
    await writeFile(join(source, 'Cargo.toml'), '');
    await binary(join(source, 'target/release')); await binary(join(installed, 'bin'));
    await binary(join(legacy, 'bin')); await binary(join(explicit, 'bin'));
    await writeFile(join(config, 'install.json'), JSON.stringify({ bin_dir: join(legacy, 'bin') }), { mode: 0o600 });
    const select = async (repo, local, prefix, override = false) => (await shell(
      'CP_REPO_ROOT=$1; CP_DEFAULT_BIN_DIR=$2; config_dir=$3; prefix=$4; prefix_explicit=$5; find_bins; printf "%s" "$bin_dir"',
      [repo, local, config, prefix, String(override)])).stdout;
    assert.equal(await select(source, source, explicit), join(source, 'target/release'));
    assert.equal(await select(source, source, explicit, true), join(explicit, 'bin'));
    assert.equal(await select(installed, join(installed, 'bin'), explicit), join(installed, 'bin'));
    assert.equal(await select(directory, directory, explicit), join(legacy, 'bin'));
    await writeFile(join(installed, 'BUNDLE.json'), '{}');
    assert.equal(await select(installed, join(installed, 'bin'), explicit, true), join(installed, 'bin'));
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('process identity handles spaces/parentheses, missing processes and recorded executable paths', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-process-'));
  const child = spawn(process.execPath, ['-e', 'process.title="cp worker) one"; process.stdout.write(String(process.pid)+"\\n"); setInterval(()=>{},1000)'],
    { stdio: ['ignore', 'pipe', 'ignore'] });
  const closed = new Promise(ok => child.once('close', ok));
  try {
    await new Promise((ok, no) => { child.stdout.once('data', ok); child.once('error', no); });
    const stat = await readFile(`/proc/${child.pid}/stat`, 'utf8');
    const ticks = stat.slice(stat.lastIndexOf(') ') + 2).trim().split(/\s+/)[19];
    assert.equal((await shell('process_start_ticks "$1"', [String(child.pid)])).stdout.trim(), ticks);
    await assert.rejects(shell('process_start_ticks 999999999'), failure => failure.stderr === '');
    const record = join(directory, 'process.json');
    await writeFile(record, JSON.stringify({ pid: child.pid, start_ticks: ticks, executable: process.execPath }), { mode: 0o600 });
    const matches = () => shell('config_dir=$1; bin_dir=/stale/installation; process_matches', [directory]);
    await matches();
    await writeFile(record, JSON.stringify({ pid: child.pid, start_ticks: '0', executable: process.execPath }));
    await assert.rejects(matches());
  } finally { child.kill('SIGTERM'); await closed; await rm(directory, { recursive: true, force: true }); }
});

test('NER device and per-device batch configuration persists, normalizes and forwards without changing CPU threads', { timeout: 20000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-ner-config-'));
  const config = join(directory, 'control'), prefix = join(directory, 'prefix');
  const server = createServer();
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  const port = server.address().port;
  await new Promise(ok => server.close(ok));
  const call = (...args) => exec(join(root, 'cpctl'), [...args, '--config-dir', config, '--prefix', prefix], { timeout: 10000 });
  try {
    await binary(join(prefix,'bin'), '#!/usr/bin/env bash\nprintf "%s\\n" "$@"\nexit 64\n');
    await call('init','--upstream','http://127.0.0.1:19999','--model-id','fixture','--no-key','--port',String(port),
      '--ner-device','cuda','--ner-batch-size','4','--ner-threads','4');
    const before = await readFile(join(config,'config.json'),'utf8');
    const settings = JSON.parse(before);
    assert.equal(settings.ner_device,'cuda:0'); assert.equal(settings.ner_batch_size,4); assert.equal(settings.ner_threads,4);
    await assert.rejects(call('start','--ner-device','cuda:0,1,2,3','--ner-batch-size','8'), failure => /退出码：64/.test(failure.stderr));
    const forwarded = (await readFile(join(config,'service.log'),'utf8')).trim().split('\n');
    for (const [flag,value] of [['--ner-device','cuda:0,1,2,3'],['--ner-batch-size','8'],['--ner-threads','4']]) {
      assert.equal(forwarded[forwarded.indexOf(flag)+1],value);
    }
    assert.equal(await readFile(join(config,'config.json'),'utf8'),before);
    await assert.rejects(call('start','--ner-device','cann','--ner-batch-size','4'), failure => /退出码：64/.test(failure.stderr));
    const cann = (await readFile(join(config,'service.log'),'utf8')).trim().split('\n');
    assert.equal(cann[cann.indexOf('--ner-device')+1],'cann:0');
    await assert.rejects(call('start','--ner-device','cann:0,1','--ner-batch-size','4'), failure => /退出码：64/.test(failure.stderr));
    const multiCann = (await readFile(join(config,'service.log'),'utf8')).trim().split('\n');
    assert.equal(multiCann[multiCann.indexOf('--ner-device')+1],'cann:0,1');

    for (const device of ['cuda:','cuda:0,0','cuda:0,00','cuda:-1','cuda:0,','cuda:2147483648','cpu:0','cann:0,0']) {
      await assert.rejects(call('start','--ner-device',device), failure => /NER 设备/.test(failure.stderr));
    }
    for (const size of ['0','257','4.5']) {
      await assert.rejects(call('start','--ner-batch-size',size), failure => /NER batch/.test(failure.stderr));
    }
    await writeFile(join(config,'config.json'), JSON.stringify({...settings,ner_device:false}));
    await assert.rejects(call('start'),failure=>/配置格式错误/.test(failure.stderr));
    // Existing user configurations without new fields still select CPU/batch 1.
    delete settings.ner_device; delete settings.ner_batch_size;
    await writeFile(join(config,'config.json'),JSON.stringify(settings));
    await assert.rejects(call('start'),failure=>/退出码：64/.test(failure.stderr));
    const legacy=(await readFile(join(config,'service.log'),'utf8')).trim().split('\n');
    assert.equal(legacy[legacy.indexOf('--ner-device')+1],'cpu');
    assert.equal(legacy[legacy.indexOf('--ner-batch-size')+1],'1');
  } finally { await rm(directory,{recursive:true,force:true}); }
});

test('fast startup failure reports the chosen executable; occupied HTTP port preserves ownership', { timeout: 15000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-start-'));
  const config = join(directory, 'control'), prefix = join(directory, 'prefix');
  const server = createServer((request, response) => { response.writeHead(401); response.end(); });
  await new Promise(ok => server.listen(0, '127.0.0.1', ok));
  const port = server.address().port;
  await new Promise(ok => server.close(ok));
  const call = (...args) => exec(join(root, 'cpctl'), [...args, '--config-dir', config, '--prefix', prefix], { timeout: 10000 });
  try {
    await binary(join(prefix, 'bin'));
    await call('init', '--upstream', 'http://127.0.0.1:19999', '--model-id', 'fixture', '--no-key', '--port', String(port));
    await assert.rejects(call('start', '--ner-threads', '4'), failure => {
      assert.match(failure.stdout, new RegExp('启动程序：' + prefix));
      assert.match(failure.stderr, /退出码：64/);
      assert.doesNotMatch(failure.stderr, /awk:|\/proc\//);
      return true;
    });
    await new Promise(ok => server.listen(port, '127.0.0.1', ok));
    const ownership = JSON.stringify({ pid: 999999999, start_ticks: '1', executable: '/prior/executable' });
    await writeFile(join(config, 'process.json'), ownership, { mode: 0o600 });
    await writeFile(join(config, 'service.log'), 'prior diagnostic', { mode: 0o600 });
    await assert.rejects(call('start'), failure => /端口.*已有 HTTP 服务/.test(failure.stderr));
    assert.equal(await readFile(join(config, 'process.json'), 'utf8'), ownership);
    assert.equal(await readFile(join(config, 'service.log'), 'utf8'), 'prior diagnostic');
  } finally {
    server.closeAllConnections(); await new Promise(ok => server.close(ok));
    await rm(directory, { recursive: true, force: true });
  }
});
