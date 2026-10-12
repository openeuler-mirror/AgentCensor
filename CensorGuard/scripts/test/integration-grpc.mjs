#!/usr/bin/env node
// Real daemon/adapter contract, event filtering and idle subscription cancellation.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtempSync, writeFileSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawn, execFileSync } from 'node:child_process';
import { createServer, createConnection } from 'node:net';
import { setTimeout as sleep } from 'node:timers/promises';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const require = createRequire(join(repo, 'plugins/dsh-censorguard/package.json'));
const grpc = require('@grpc/grpc-js');
const loader = require('@grpc/proto-loader');
const definition = loader.loadSync(join(repo, 'api/censorguard/v1/censorguard.proto'), {
  longs: String, defaults: true,
});
const Client = grpc.loadPackageDefinition(definition).censorguard.v1.Censorguard;
const directory = mkdtempSync(join(tmpdir(), 'cg-grpc-'));
console.log(`Artifacts: ${directory}`);
const config = join(directory, 'base.yaml');
const group = join(directory, 'group.yaml');
const secret = join(directory, 'secret');
let daemon, client, trackedRoot, pluginClient;
const streams = [];

function run(binary, ...args) {
  return execFileSync(join(repo, 'target/debug', binary), ['--config', config, ...args], {
    encoding: 'utf8', timeout: 40000, stdio: 'pipe',
  });
}
function ctl(...args) { return JSON.parse(run('censorguardctl', ...args)); }
function unary(method, request = {}) {
  return new Promise((resolveReply, reject) => client[method](request, {
    deadline: Date.now() + 10000,
  }, (error, reply) => error ? reject(error) : resolveReply(reply)));
}
async function waitFor(check, message, timeout = 10000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (await check()) return;
    await sleep(50);
  }
  throw new Error(message);
}
async function denied(method, request = {}, code = grpc.status.PERMISSION_DENIED) {
  await assert.rejects(unary(method, request), error => error.code === code);
}
function subscribe(request) {
  const items = [];
  const stream = client.SubscribeEvents(request);
  stream.on('data', event => items.push(event));
  stream.on('error', error => {
    if (error.code !== grpc.status.CANCELLED) items.push({ error: String(error) });
  });
  streams.push(stream);
  return { stream, items };
}

function rawApply(expectedRevision, policyYaml, index) {
  return new Promise((resolveReply, reject) => {
    const socket = createConnection(join(directory, 'ctl.sock'));
    socket.setTimeout(10000, () => socket.destroy(new Error('raw RPC timeout')));
    socket.on('error', reject);
    socket.on('connect', () => socket.write(JSON.stringify({
      v: 2, request_id: `race-${index}`, method: 'apply_policy',
      params: { group: 'censorguard-dsh-default', expected_revision: expectedRevision, policy_yaml: policyYaml },
    }) + '\n'));
    let buffer = '';
    socket.on('data', data => {
      buffer += data;
      if (buffer.includes('\n')) { socket.destroy(); resolveReply(JSON.parse(buffer.split('\n')[0])); }
    });
  });
}

try {
  const picker = createServer();
  await new Promise(resolveListen => picker.listen(0, '127.0.0.1', resolveListen));
  const port = picker.address().port;
  await new Promise(resolveClose => picker.close(resolveClose));
  writeFileSync(secret, 'secret\n');
  writeFileSync(group, 'rules: []\n');
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
  grpc_listen: 127.0.0.1:${port}
  audit_file: true
  audit_exec: false
  audit_net: true
rules:
  - file deny ${secret}
`);
  let log = '';
  daemon = spawn(join(repo, 'target/debug/censorguardd'), ['launch', '--config', config]);
  for (const output of [daemon.stdout, daemon.stderr]) output.on('data', data => {
    log += data; writeFileSync(join(directory, 'daemon.log'), log);
  });
  await waitFor(() => {
    assert.equal(daemon.exitCode, null, log);
    return log.includes('required hooks attached') && /gRPC adapter ready: pid=\d+/.test(log);
  }, 'daemon not ready');
  const adapterPid = Number(log.match(/gRPC adapter ready: pid=(\d+)/)[1]);
  client = new Client(`127.0.0.1:${port}`, grpc.credentials.createInsecure());
  await new Promise((resolveReady, reject) => client.waitForReady(Date.now() + 10000,
    error => error ? reject(error) : resolveReady()));
  const status = () => ctl('status', '--json');

  // No tracked activity: cancelled idle streams must release both adapter threads and daemon subscribers.
  const threads = () => readdirSync(`/proc/${adapterPid}/task`).length;
  const fds = () => readdirSync(`/proc/${adapterPid}/fd`).length;
  const initialThreads = threads();
  const initialFds = fds();
  for (let i = 0; i < 24; i++) {
    const { stream } = subscribe({ kinds: [2], allowed: false });
    await waitFor(() => status().event_subscribers > 0, 'subscription was not installed');
    stream.cancel();
    await waitFor(() => status().event_subscribers === 0, 'cancelled subscription was not released');
  }
  await waitFor(() => threads() <= initialThreads + 2 && fds() <= initialFds + 2
    && status().event_subscribers === 0,
  `idle subscription leak: threads=${threads()}, fds=${fds()}, subscribers=${status().event_subscribers}`);
  console.log('PASS cancelled idle subscriptions release adapter threads, descriptors and daemon subscribers');

  const expected = { enable_file: true, enable_exec: true, enable_net: true,
    audit_file: true, audit_exec: false, audit_net: true };
  assert.deepEqual((await unary('Status')).config, expected);
  assert.deepEqual((await unary('GetConfig')).config, expected);
  for (const method of ['Track', 'Untrack', 'SetConfig', 'SetDomainBinding', 'RemovePolicy', 'SetSwitches']) {
    await denied(method);
  }
  await denied('ApplyPolicy', { name: '__base__', policyYaml: 'rules: []\n' });
  await denied('ApplyPolicy', { name: 'outside', policyYaml: 'rules: []\n' });
  await denied('ValidatePolicy', { name: 'outside', policyYaml: 'rules: []\n' });
  await denied('GetPolicy', { name: 'missing' }, grpc.status.NOT_FOUND);
  assert((await unary('GetPolicy', { name: '__base__' })).rules.includes(`file deny ${secret}`));
  console.log('PASS six live switches, read endpoints, admin/baseline/whitelist denials and error mapping');

  ctl('policy', 'apply', '--policy', 'censorguard-dsh-default', '--file', group);
  ctl('bind', '--domain', 'grpc-test', '--policy', 'censorguard-dsh-default');
  trackedRoot = spawn('/usr/bin/sleep', ['120']);
  ctl('attach', '--pid', String(trackedRoot.pid), '--domain', 'grpc-test');
  const revision = status().version;
  const yaml = `rules:\n  - exec deny /usr/bin/true\n`;
  const invalid = await unary('ValidatePolicy', { name: 'censorguard-dsh-default', policyYaml: 'rules: [' });
  assert.equal(invalid.ok, false);
  assert(invalid.errors.length);
  const validation = await unary('ValidatePolicy', { name: 'censorguard-dsh-default', policyYaml: yaml });
  assert.equal(validation.ok, true);
  assert.deepEqual(validation.affected, ['grpc-test']);
  const preview = await unary('ApplyPolicy', { name: 'censorguard-dsh-default', policyYaml: yaml, dryRun: true });
  assert.deepEqual(preview.affected, ['grpc-test']);
  assert.equal(status().version, revision);
  const applied = await unary('ApplyPolicy', { name: 'censorguard-dsh-default', policyYaml: yaml });
  assert.deepEqual(applied.affected, ['grpc-test']);
  assert.equal(applied.version, revision + 1);
  assert.equal((await unary('GetPolicy', { name: 'censorguard-dsh-default' })).version, applied.version);
  assert.throws(() => run('censorguardctl', 'spawn', '--domain', 'grpc-test', '--', '/usr/bin/true'));
  console.log('PASS invalid/valid validation, dry-run immutability, affected domains and actual group enforcement');

  const parallelRevision = status().version;
  const { GrpcClient } = await import(pathToFileURL(join(repo, 'plugins/dsh-censorguard/dist/host/grpc.js')));
  pluginClient = new GrpcClient(`127.0.0.1:${port}`, join(repo, 'plugins/dsh-censorguard/proto/censorguard/v1/censorguard.proto'));
  const updates = await Promise.allSettled(Array.from({ length: 8 }, () => pluginClient.applyPolicy(
    'censorguard-dsh-default', yaml, false, undefined, parallelRevision)));
  assert.equal(updates.filter(result => result.status === 'fulfilled').length, 1, 'stale concurrent gRPC writers must be rejected');
  assert(updates.filter(result => result.status === 'rejected').every(result => result.reason.code === grpc.status.FAILED_PRECONDITION));
  assert.equal(status().version, parallelRevision + 1);
  const rawRevision = status().version;
  const rawUpdates = await Promise.all(Array.from({ length: 8 }, (_, index) => rawApply(rawRevision, yaml, index)));
  assert.equal(rawUpdates.filter(result => result.ok).length, 1, 'revision check must share the daemon update lock');
  assert(rawUpdates.filter(result => !result.ok).every(result => result.error.code === 'revision_conflict'));
  assert.equal(status().version, rawRevision + 1);
  console.log('PASS concurrent gRPC/raw group updates accept one writer and reject seven stale revisions');

  try {
    const id = status().domains.find(domain => domain.name === 'grpc-test').id;
    const selected = subscribe({ domainIds: [String(id)], kinds: [1], allowed: false });
    const excluded = subscribe({ domainIds: ['18446744073709551615'], kinds: [1], allowed: false });
    await waitFor(() => status().event_subscribers === 2, 'filtered streams not installed');
    assert.throws(() => run('censorguardctl', 'spawn', '--domain', 'grpc-test', '--', '/usr/bin/cat', secret));
    await waitFor(() => selected.items.some(event => event.detail.includes('secret')), 'filtered DENY missing');
    const event = selected.items.find(item => item.detail.includes('secret'));
    assert.equal(event.kind, 1);
    assert.equal(event.allowed, false);
    assert.equal(event.domain, 'grpc-test');
    assert(BigInt(event.sequence) > 0n);
    assert.equal(event.daemonBootId, status().daemon_boot_id);
    assert.equal(excluded.items.length, 0);
    selected.stream.cancel(); excluded.stream.cancel();
    await waitFor(() => status().event_subscribers === 0, 'filtered subscriptions not released');
    writeFileSync(join(directory, 'event.json'), JSON.stringify(event, null, 2));
  } finally { trackedRoot.kill(); }
  console.log('PASS domain/kind/decision event filtering, sequence/boot identity and cancellation');
  execFileSync(join(repo, 'target/debug/censorguardd'), ['stop', '--config', config], { timeout: 40000 });
} finally {
  for (const stream of streams) stream.cancel();
  trackedRoot?.kill();
  client?.close();
  pluginClient?.dispose();
  if (daemon?.exitCode === null) {
    daemon.kill();
    await waitFor(() => daemon.exitCode !== null, 'daemon cleanup timed out');
  }
}
