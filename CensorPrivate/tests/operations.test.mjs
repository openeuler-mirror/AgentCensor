// Operational acceptance runs the shipped commands and installed layout.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, copyFile, readFile, writeFile, stat, chmod, symlink, unlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const exec = promisify(execFile);
const root = resolve(import.meta.dirname, '..');
test('installed censorprivacy: initialization, lifecycle, credential routing and fail closed', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-ops-test-'));
  const config = join(directory, 'control'); const prefix = join(directory, 'prefix');
  let received = 0; const captured = []; const providerKey = 'fictional-provider-credential-for-test';
  const upstream = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks));
    captured.push(body);
    assert.equal(request.headers.authorization, `Bearer ${providerKey}`); received++;
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Connection', 'close');
    if (body.messages.some(message => message.role === 'user' && message.content.includes('cp_custom_sort_probe'))) {
      const operands = body.messages.flatMap(message => message.role === 'user' ? message.content.match(/<CP_CUSTOMER_[a-z0-9]{8}>/g) || [] : []);
      if (operands.length !== 2) { response.writeHead(400); response.end('{}'); return; }
      const tool = body.messages.find(message => message.role === 'tool');
      if (tool) {
        response.end(JSON.stringify({ choices: [{ index: 0, message: { role: 'assistant', content: operands.toReversed().join(', ') }, finish_reason: 'stop' }] }));
      } else {
        const tool_calls = [{ id: 'call_local_sort', type: 'function',
          function: { name: 'private_sort', arguments: JSON.stringify({ refs: operands }) } }];
        response.end(JSON.stringify({ choices: [{ index: 0, message: { role: 'assistant', content: null, tool_calls }, finish_reason: 'tool_calls' }] }));
      }
      return;
    }
    const reasoning = body.model.startsWith('deepseek-') && body.thinking?.type !== 'disabled';
    response.end(JSON.stringify({ choices: [{ index: 0, message: { role: 'assistant', content: 'OK',
      ...(reasoning ? { reasoning_content: 'synthetic default reasoning' } : {}) }, finish_reason: 'stop' }] }));
  });
  await new Promise(ok => upstream.listen(0, '127.0.0.1', ok));
  const reserve = createServer(); await new Promise(ok => reserve.listen(0, '127.0.0.1', ok));
  const port = reserve.address().port; await new Promise(ok => reserve.close(ok));
  const command = join(prefix, 'bin/censorprivacy');
  const call = (operation, ...args) => exec(command, [operation, '--config-dir', config, ...args], { cwd: directory, timeout: 60000, maxBuffer: 1024 * 1024 });
  try {
    await exec(join(root, 'censorprivacy'), ['install', '--tls', '--prefix', prefix, '--config-dir', config], { cwd: root, timeout: 60000 });
    assert.ok((await stat(join(prefix, 'bin/censorprivacy'))).mode & 0o111);
    for (const legacy of ['cpctl', 'privacy-run', 'privcalc', 'censorprivacy-mcp', 'tls-run']) {
      await assert.rejects(stat(join(prefix, 'bin', legacy)), { code: 'ENOENT' });
    }
    await stat(join(prefix, 'share/censorprivacy/docs/usage.md'));
    await stat(join(prefix, 'share/censorprivacy/config.yaml'));
    await assert.rejects(stat(join(prefix, 'share/censorprivacy/examples/policy.yaml')), { code: 'ENOENT' });
    await assert.rejects(stat(join(prefix, 'share/censorprivacy/policy.example.yaml')), { code: 'ENOENT' });
    const installedSettings = JSON.parse(await readFile(join(prefix, 'share/censorprivacy/config.yaml'), 'utf8'));
    const sourceSettings = JSON.parse((await exec(join(root, 'target/release/censorprivacy'),
      ['config-check', '--config', join(root, 'config.yaml'), '--print-resolved'])).stdout);
    assert.equal(installedSettings.ner.model_dir, null);
    assert.equal(installedSettings.server.token_file, null);
    assert.equal(installedSettings.server.ttl_seconds, 86400);
    assert.deepEqual(installedSettings.detection.categories, sourceSettings.detection.categories);
    await stat(join(prefix, 'share/censorprivacy/docs/configuration.md'));
    await stat(join(prefix, 'share/censorprivacy/LICENSE'));
    await stat(join(prefix, 'share/censorprivacy/docs/rust-detector.md'));
    await assert.rejects(stat(join(prefix, 'share/censorprivacy/detector/worker.py')), { code: 'ENOENT' });
    for (const directory of ['offline', 'tools', 'scripts/model', 'licenses']) {
      await assert.rejects(stat(join(prefix, 'share/censorprivacy', directory)), { code: 'ENOENT' });
    }
    assert.equal((await stat(join(prefix, 'share/censorprivacy'))).mode & 0o777, 0o755);
    await stat(join(prefix, 'share/censorprivacy/plugins/dsh-censorprivacy/index.mjs'));
    const key = join(directory, 'provider.key'); await writeFile(key, providerKey, { mode: 0o600 });
    await call('init', '--upstream', `http://127.0.0.1:${upstream.address().port}`, '--model-id', 'deepseek-flash', '--key-file', key, '--port', String(port),'--ner-threads','4');
    const before = await readFile(join(config, 'config.json'), 'utf8');
    assert.equal(JSON.parse(before).ner_threads,4);
    await call('init'); assert.equal(await readFile(join(config, 'config.json'), 'utf8'), before);
    await writeFile(join(config,'config.json'),JSON.stringify({...JSON.parse(before),ner_threads:false}));
    await assert.rejects(call('start'),failure=>/配置格式错误/.test(failure.stderr));
    await writeFile(join(config,'config.json'),before);
    assert.equal((await stat(config)).mode & 0o777, 0o700);
    for (const file of ['config.json', 'upstream.key', 'gateway.token', 'curl.headers']) assert.equal((await stat(join(config, file))).mode & 0o777, 0o600);
    await assert.rejects(stat(join(config, 'policy.yaml')), { code: 'ENOENT' });
    if (process.env.CP_TEST_DSH_ROOT) {
      await writeFile(join(config, 'private-tools.json'), JSON.stringify({ tools: [{
        name: 'private_sort', description: 'Sort the provided private text references.',
        command: ['/usr/bin/python3', '-I', join(prefix, 'share/censorprivacy/examples/private-tools/sort.py')],
        output: 'order',
      }] }), { mode: 0o600 });
    }
    await writeFile(join(prefix, 'share/censorprivacy/config.yaml'), 'server: {ttl_seconds: 86400}\ncustom: {epoch: 1, dictionaries: {CUSTOMER: [Fictional Zebra, Fictional Apple]}}\ndetection: {categories: all}\nner: {threads: 3}\n');
    await call('start'); await call('check', '--model'); assert.equal(received, 1);
    assert.deepEqual(captured[0].thinking, { type: 'disabled' });
    if (process.env.CP_TEST_DSH_ROOT) {
      await call('dsh', 'setup', '--dsh-path', process.env.CP_TEST_DSH_ROOT);
      const literalTask = '--help --root /not-a-real-directory should be ordinary task text';
      const result = await call('dsh', 'run', '--', literalTask).catch(async failure => {
        failure.message += '\nDSH stderr:\n' + await readFile(join(config, 'dsh/dsh-stderr.txt'), 'utf8');
        failure.message += '\nCaptured request metadata:\n' + JSON.stringify(captured.map(body => ({ model: body.model, thinking: body.thinking })));
        throw failure;
      });
      assert.equal(result.stdout.trim(), 'OK');
      assert.ok(captured.at(-1).messages.some(message => message.role === 'user' && message.content.includes(literalTask)));
      assert.deepEqual(captured.at(-1).tools.map(tool => tool.function.name).sort(), ['private_compute', 'private_sort']);
      const beforeSort = captured.length;
      const sort = await call('dsh', 'run', 'cp_custom_sort_probe Fictional Zebra; Fictional Apple');
      assert.equal(sort.stdout.trim(), 'Fictional Apple, Fictional Zebra');
      const sortRequests = captured.slice(beforeSort);
      assert.equal(sortRequests.length, 2);
      for (const body of sortRequests) {
        assert.ok(!JSON.stringify(body).includes('Fictional Zebra') && !JSON.stringify(body).includes('Fictional Apple'));
        assert.deepEqual(body.tools.map(tool => tool.function.name).sort(), ['private_compute', 'private_sort']);
      }
      const originalRefs = sortRequests[0].messages.flatMap(message => message.role === 'user'
        ? message.content.match(/<CP_CUSTOMER_[a-z0-9]{8}>/g) || [] : []);
      const toolResult = sortRequests[1].messages.find(message => message.role === 'tool');
      assert.deepEqual(JSON.parse(toolResult.content), { refs: originalRefs.toReversed() });
      await call('dsh', 'run', '--enable-tools', 'Reply with OK and do not use tools.');
      assert.ok(captured.at(-1).tools.some(tool => !['private_compute', 'private_sort'].includes(tool.function.name)));
      assert.ok(captured.at(-1).tools.some(tool => tool.function.name === 'private_compute'));
      await assert.rejects(call('dsh', 'run', '--enable-tools', '--no-tools', 'task'));
    }
    await chmod(config, 0o755);
    await assert.rejects(call('check'), failure => /0700/.test(failure.stderr));
    await chmod(config, 0o700);
    await assert.rejects(exec(join(root, 'censorprivacy'), ['install', '--prefix', prefix, '--config-dir', config], { cwd: root, timeout: 20000 }));
    const firstRun = JSON.parse(await readFile(join(config, 'run.json'))).run_id;
    await assert.rejects(call('start'));
    // A stale process record must never authorize signalling a reused PID.
    const processPath = join(config, 'process.json'); const ownedProcess = await readFile(processPath, 'utf8');
    const stale = JSON.parse(ownedProcess); stale.start_ticks = '0'; await writeFile(processPath, JSON.stringify(stale));
    assert.match((await call('stop')).stdout, /不发送信号/);
    const token = (await readFile(join(config, 'gateway.token'), 'utf8')).trim();
    const ready=await fetch(`http://127.0.0.1:${port}/ready`, { headers: { authorization: `Bearer ${token}` } });
    assert.equal(ready.status,200);
    const initialNer = await ready.json();
    assert.equal(initialNer.ner_threads,3); assert.equal(initialNer.ner_device,'cpu'); assert.equal(initialNer.ner_batch_size,1);
    await assert.rejects(call('start','--ner-threads','0'));
    await assert.rejects(call('start','--ner-threads','4.5'));
    await writeFile(processPath, ownedProcess); await call('stop'); await call('start','--ner-device','cpu','--ner-threads','2','--ner-batch-size','2');
    const overrideNer = await (await fetch(`http://127.0.0.1:${port}/ready`,{headers:{authorization:`Bearer ${token}`}})).json();
    assert.equal(overrideNer.ner_threads,2); assert.equal(overrideNer.ner_device,'cpu'); assert.equal(overrideNer.ner_batch_size,2);
    assert.equal(await readFile(join(config,'config.json'),'utf8'),before,'a start override must not rewrite persisted configuration');
    assert.notEqual(JSON.parse(await readFile(join(config, 'run.json'))).run_id, firstRun);
    await assert.rejects(call('dsh', 'cloud')); // A loopback simulation cannot be reported as cloud acceptance.
    // Exercise the fixture's argv and authenticated readiness without starting
    // a second ONNX model alongside the managed gateway or contacting a provider.
    const fakePrefix = join(directory, 'fixture-prefix');
    const fakeServer = join(fakePrefix, 'bin/censorprivacy');
    const mismatchMarker = join(directory, 'force-mismatched-readiness');
    await mkdir(join(fakePrefix, 'bin'), { recursive: true });
    await writeFile(fakeServer, `#!${process.execPath}
const fs = require('node:fs'), http = require('node:http');
const value = arg => process.argv[process.argv.indexOf(arg) + 1];
const run_id = 'fixture-run';
const auth = 'Bearer ' + fs.readFileSync(value('--token-file'), 'utf8');
fs.writeFileSync(value('--run-state-file'), JSON.stringify({run_id}), {mode: 0o600});
http.createServer((req, res) => {
  if (req.url !== '/ready' || req.headers.authorization !== auth) { res.writeHead(401).end(); return; }
  res.end(JSON.stringify({status: 'ready', run_id,
    ner_threads: Number(value('--ner-threads')) + Number(fs.existsSync(${JSON.stringify(mismatchMarker)})),
    ner_device: value('--ner-device'),
    ner_batch_size: Number(value('--ner-batch-size')),
  }));
}).listen(Number(value('--port')), '127.0.0.1');
`, { mode: 0o700 });
    const customNative = join(directory, 'custom-tls-guard.so');
    await copyFile(join(prefix, 'lib/censorprivacy/libcensorprivacy_tls_guard.so'), customNative);
    const fakeDsh = join(directory, 'failing-dsh');
    const fakeEntry = join(fakeDsh, 'apps/cli/lib/bin.js');
    const capturedPatch = join(directory, 'cloud-patch.yml');
    await mkdir(join(fakeDsh, 'apps/cli/lib'), { recursive: true });
    await writeFile(fakeEntry, `const fs = require('node:fs');
const patch = process.argv[process.argv.indexOf('--patch') + 1];
fs.copyFileSync(patch, ${JSON.stringify(capturedPatch)});
process.exit(7);
`);
    const cloudConfig = { ...JSON.parse(before), mode: 'tls', upstream: 'https://192.0.2.1', native_library: customNative };
    await writeFile(join(config, 'config.json'), JSON.stringify(cloudConfig));
    const diagnoses = [];
    const requestsBeforeCloud = received;
    try {
      await assert.rejects(call('dsh', 'cloud', '--dsh-root', fakeDsh, '--prefix', fakePrefix), failure => { diagnoses.push(failure); return true; });
      const patch = await readFile(capturedPatch, 'utf8');
      assert.ok(patch.includes(`nativeLibrary: ${JSON.stringify(customNative)}`));
      const reportPath = join(config, 'dsh/cloud-report.json');
      const report = JSON.parse(await readFile(reportPath, 'utf8'));
      assert.equal(report.status, 'failed');
      assert.equal(report.mode, 'tls');
      const expectedNer = { ner_threads: 2, ner_device: 'cpu', ner_batch_size: 2 };
      assert.deepEqual(report.ner_requested, expectedNer);
      assert.deepEqual(report.ner_effective, expectedNer);
      assert.equal((await stat(reportPath)).mode & 0o777, 0o600);
      assert.ok(!JSON.stringify(report).includes(providerKey));
      assert.equal(received, requestsBeforeCloud);
      await unlink(capturedPatch);
      await writeFile(mismatchMarker, '');
      await assert.rejects(call('dsh', 'cloud', '--dsh-path', fakeDsh, '--prefix', fakePrefix), failure => {
        diagnoses.push(failure);
        return /cloud_gateway_ner_settings_mismatch/.test(failure.stderr);
      });
      const mismatch = JSON.parse(await readFile(reportPath, 'utf8'));
      assert.equal(mismatch.reason, 'cloud_gateway_ner_settings_mismatch');
      assert.deepEqual(mismatch.ner_requested, expectedNer);
      assert.deepEqual(mismatch.ner_effective, { ...expectedNer, ner_threads: 3 });
      await assert.rejects(stat(capturedPatch), { code: 'ENOENT' });
    } finally {
      for (const failure of diagnoses) {
        for (const match of String(failure.stdout).matchAll(/^私有诊断目录：(\/tmp\/cp-demo-[a-zA-Z0-9]+)$/gm)) {
          await rm(match[1], { recursive: true, force: true });
        }
      }
      await writeFile(join(config, 'config.json'), before);
    }
    const changed = JSON.parse(before); changed.upstream += '\nCP_MODEL=hostile'; await writeFile(join(config, 'config.json'), JSON.stringify(changed));
    await assert.rejects(call('start'));
    await call('stop'); // A broken model configuration must not prevent stopping the owned process.
    await writeFile(join(config, 'config.json'), before);
    const headersPath = join(config, 'curl.headers'); const headers = await readFile(headersPath);
    const victim = join(directory, 'victim'); await writeFile(victim, 'preserve', { mode: 0o600 });
    await unlink(headersPath); await symlink(victim, headersPath);
    await assert.rejects(call('check')); assert.equal(await readFile(victim, 'utf8'), 'preserve');
    await unlink(headersPath); await writeFile(headersPath, headers, { mode: 0o600 });
    await chmod(join(config, 'config.json'), 0o644); await assert.rejects(call('start')); await chmod(join(config, 'config.json'), 0o600);
    await assert.rejects(call('start', '--dry-run'));
    const configuredYaml = await readFile(join(prefix, 'share/censorprivacy/config.yaml'), 'utf8');
    await exec(join(root, 'censorprivacy'), ['install', '--tls', '--prefix', prefix, '--config-dir', config], { cwd: root, timeout: 60000 });
    assert.equal(await readFile(join(prefix, 'share/censorprivacy/config.yaml'), 'utf8'), configuredYaml);
    assert.match((await call('demo', 'gateway')).stdout, /通过/);
    assert.match((await call('demo', 'tls')).stdout, /通过/);
  } finally {
    await chmod(config, 0o700).catch(() => {});
    await call('stop').catch(() => {});
    upstream.closeAllConnections(); await new Promise(ok => upstream.close(ok));
    await rm(directory, { recursive: true, force: true });
  }
});
