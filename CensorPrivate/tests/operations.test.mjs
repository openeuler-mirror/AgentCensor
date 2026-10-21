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
test('installed cpctl: initialization, lifecycle, credential routing and fail closed', { timeout: 90000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'cp-ops-test-'));
  const config = join(directory, 'control'); const prefix = join(directory, 'prefix');
  let received = 0; const captured = []; const providerKey = 'fictional-provider-credential-for-test';
  const upstream = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    captured.push(JSON.parse(Buffer.concat(chunks)));
    assert.equal(request.headers.authorization, `Bearer ${providerKey}`); received++;
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ index: 0, message: { role: 'assistant', content: 'OK' }, finish_reason: 'stop' }] }));
  });
  await new Promise(ok => upstream.listen(0, '127.0.0.1', ok));
  const reserve = createServer(); await new Promise(ok => reserve.listen(0, '127.0.0.1', ok));
  const port = reserve.address().port; await new Promise(ok => reserve.close(ok));
  const command = join(prefix, 'bin/cpctl');
  const call = (operation, ...args) => exec(command, [operation, '--config-dir', config, ...args], { cwd: directory, timeout: 20000, maxBuffer: 1024 * 1024 });
  try {
    await exec(join(root, 'cpctl'), ['install', '--tls', '--prefix', prefix, '--config-dir', config], { cwd: root, timeout: 60000 });
    for (const binary of ['censorprivacy', 'privacy-run', 'privcalc', 'censorprivacy-mcp', 'tls-run', 'cpctl']) assert.ok((await stat(join(prefix, 'bin', binary))).mode & 0o111);
    await stat(join(prefix, 'share/censorprivacy/docs/usage.md'));
    assert.equal((await stat(join(prefix, 'share/censorprivacy'))).mode & 0o777, 0o755);
    await stat(join(prefix, 'share/censorprivacy/plugins/dsh-censorprivacy/index.mjs'));
    const key = join(directory, 'provider.key'); await writeFile(key, providerKey, { mode: 0o600 });
    await call('init', '--upstream', `http://127.0.0.1:${upstream.address().port}`, '--model-id', 'test-model', '--key-file', key, '--port', String(port));
    const before = await readFile(join(config, 'config.json'), 'utf8');
    await call('init'); assert.equal(await readFile(join(config, 'config.json'), 'utf8'), before);
    assert.equal((await stat(config)).mode & 0o777, 0o700);
    for (const file of ['config.json', 'policy.yaml', 'upstream.key', 'gateway.token', 'curl.headers']) assert.equal((await stat(join(config, file))).mode & 0o777, 0o600);
    await call('start'); await call('check', '--model'); assert.equal(received, 1);
    if (process.env.CP_TEST_DSH_ROOT) {
      await call('dsh', 'setup', '--dsh-root', process.env.CP_TEST_DSH_ROOT);
      const literalTask = '--help --root /not-a-real-directory should be ordinary task text';
      const result = await call('dsh', 'run', '--', literalTask);
      assert.equal(result.stdout.trim(), 'OK');
      assert.ok(captured.at(-1).messages.some(message => message.role === 'user' && message.content.includes(literalTask)));
    }
    await chmod(config, 0o755);
    await assert.rejects(call('check'), failure => /0700/.test(failure.stderr));
    await chmod(config, 0o700);
    await assert.rejects(exec(join(root, 'cpctl'), ['install', '--prefix', prefix, '--config-dir', config], { cwd: root, timeout: 20000 }));
    const firstRun = JSON.parse(await readFile(join(config, 'run.json'))).run_id;
    await assert.rejects(call('start'));
    // A stale process record must never authorize signalling a reused PID.
    const processPath = join(config, 'process.json'); const ownedProcess = await readFile(processPath, 'utf8');
    const stale = JSON.parse(ownedProcess); stale.start_ticks = '0'; await writeFile(processPath, JSON.stringify(stale));
    assert.match((await call('stop')).stdout, /不发送信号/);
    const token = (await readFile(join(config, 'gateway.token'), 'utf8')).trim();
    assert.equal((await fetch(`http://127.0.0.1:${port}/ready`, { headers: { authorization: `Bearer ${token}` } })).status, 200);
    await writeFile(processPath, ownedProcess); await call('stop'); await call('start');
    assert.notEqual(JSON.parse(await readFile(join(config, 'run.json'))).run_id, firstRun);
    await assert.rejects(call('dsh', 'cloud')); // A loopback simulation cannot be reported as cloud acceptance.
    // Inspect cloud setup without contacting a provider. A deliberately failing
    // DSH stub captures the generated patch; this is never a cloud success test.
    const customNative = join(directory, 'custom-tls-guard.so');
    await copyFile(join(prefix, 'lib/censorprivacy/libcp_tls_guard.so'), customNative);
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
    let cloudFailure;
    const requestsBeforeCloud = received;
    try {
      await assert.rejects(call('dsh', 'cloud', '--dsh-root', fakeDsh), failure => { cloudFailure = failure; return true; });
      const patch = await readFile(capturedPatch, 'utf8');
      assert.ok(patch.includes(`nativeLibrary: ${JSON.stringify(customNative)}`));
      const reportPath = join(config, 'dsh/cloud-report.json');
      const report = JSON.parse(await readFile(reportPath, 'utf8'));
      assert.equal(report.status, 'failed');
      assert.equal(report.mode, 'tls');
      assert.equal((await stat(reportPath)).mode & 0o777, 0o600);
      assert.ok(!JSON.stringify(report).includes(providerKey));
      assert.equal(received, requestsBeforeCloud);
    } finally {
      for (const match of String(cloudFailure?.stdout).matchAll(/^私有诊断目录：(\/tmp\/cp-demo-[a-zA-Z0-9]+)$/gm)) {
        await rm(match[1], { recursive: true, force: true });
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
    assert.match((await call('demo', 'gateway')).stdout, /通过/);
    assert.match((await call('demo', 'tls')).stdout, /通过/);
  } finally {
    await chmod(config, 0o700).catch(() => {});
    await call('stop').catch(() => {});
    upstream.closeAllConnections(); await new Promise(ok => upstream.close(ok));
    await rm(directory, { recursive: true, force: true });
  }
});
