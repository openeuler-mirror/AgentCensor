import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile, chmod, rm, rename } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer } from 'node:net'
import {
  detectRunnerEnvironment,
  normalizeIsolationConfig,
  probeDaemonSocket,
  probeRunnerEnvironment,
} from '../src/runner-environment.js'

function baseConfig(overrides = {}) {
  return {
    socket: join(tmpdir(), 'censorfs-control-does-not-exist.sock'),
    censorfsCommand: 'censorfs',
    mounterCommand: 'censorfs-mounter',
    runnerIsolation: { mode: 'auto', minimumLevel: 'process' },
    ...overrides,
  }
}

async function fakeCgroupRoot(controllers = ['memory', 'pids', 'cpu']) {
  const root = await mkdtemp(join(tmpdir(), 'censorfs-cgroup-'))
  await writeFile(join(root, 'cgroup.controllers'), `${controllers.join(' ')}\n`)
  await writeFile(join(root, 'cgroup.procs'), '')
  // Structural v2 evidence the probe requires to exist and be readable.
  await writeFile(join(root, 'cgroup.type'), '0\n')
  await writeFile(join(root, 'cgroup.events'), 'populated 0\n')
  await writeFile(join(root, 'cgroup.subtree_control'), '')
  return root
}

async function fakeBinDir(names) {
  const bin = await mkdtemp(join(tmpdir(), 'censorfs-bin-'))
  for (const name of names) {
    await writeFile(join(bin, name), '#!/bin/sh\nexit 0\n')
    await chmod(join(bin, name), 0o755)
  }
  return bin
}

test('normalizeIsolationConfig defaults to auto/process and rejects bad values', () => {
  const config = normalizeIsolationConfig()
  assert.equal(config.mode, 'auto')
  assert.equal(config.minimumLevel, 'process')
  assert.equal(config.cleanupTimeoutMs, 5000)
  assert.equal(config.root, undefined)
  assert.throws(() => normalizeIsolationConfig({ mode: 'nope' }), /mode must be auto, required, process, or external/u)
  assert.throws(() => normalizeIsolationConfig({ minimumLevel: 'kernel' }), /minimumLevel must be process, lifecycle, or resource/u)
  assert.throws(() => normalizeIsolationConfig({ root: '/sys/fs/cgroup/x' }), /stateDir is required with root/u)
  assert.throws(() => normalizeIsolationConfig({ memoryMax: 42 }), /memoryMax must be a non-empty string/u)
  assert.throws(() => normalizeIsolationConfig({ cleanupTimeoutMs: 0 }), /cleanupTimeoutMs must be a positive safe integer/u)
  assert.throws(() => normalizeIsolationConfig(null), /runnerIsolation must be an object/u)
})

test('legacy runnerCgroup still maps to required/lifecycle and keeps root/stateDir', () => {
  const legacy = {
    enabled: true,
    root: '/sys/fs/cgroup/censorfs-runners',
    stateDir: '/run/censorfs/cgroup-state',
    memoryMax: '2147483648',
    cleanupTimeoutMs: 7000,
  }
  const config = normalizeIsolationConfig({}, legacy)
  assert.equal(config.mode, 'required')
  assert.equal(config.minimumLevel, 'lifecycle')
  assert.equal(config.root, legacy.root)
  assert.equal(config.stateDir, legacy.stateDir)
  assert.equal(config.memoryMax, legacy.memoryMax)
  assert.equal(config.cleanupTimeoutMs, 7000)
})

test('new runnerIsolation fields win over the legacy block', () => {
  const config = normalizeIsolationConfig(
    { mode: 'auto', minimumLevel: 'process', root: '/new/root', stateDir: '/new/state' },
    { enabled: true, root: '/legacy/root', stateDir: '/legacy/state' },
  )
  assert.equal(config.mode, 'auto')
  assert.equal(config.root, '/new/root')
})

test('probe detects a delegated cgroup v2 root at resource level with limits', async () => {
  const root = await fakeCgroupRoot()
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: {
      mode: 'auto',
      minimumLevel: 'resource',
      root,
      stateDir: join(root, 'state'),
      memoryMax: '1073741824',
      pidsMax: '128',
      cpuMax: '200000 100000',
    },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, true)
  assert.equal(report.effectiveLevel, 'resource')
  assert.equal(report.cgroupRoot, root)
  assert.ok(report.controllers.includes('memory'))
})

test('probe reports lifecycle when the delegated root has no requested limits', async () => {
  const root = await fakeCgroupRoot()
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'auto', minimumLevel: 'process', root, stateDir: join(root, 'state') },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, true)
  assert.equal(report.effectiveLevel, 'lifecycle')
  // Plain lifecycle with no limits configured must not spam "missing limits".
  assert.ok(!report.warnings.some((warning) => warning.includes('resource isolation requires')),
    'no-limit lifecycle must not warn about missing resource limits')
})

test('minimumLevel=resource reports missing limits even when none are configured', async () => {
  const root = await fakeCgroupRoot()
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'auto', minimumLevel: 'resource', root, stateDir: join(root, 'state') },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, true)
  assert.equal(report.effectiveLevel, 'lifecycle')
  assert.ok(report.warnings.some((warning) => warning.includes('resource isolation requires') && warning.includes('memoryMax') && warning.includes('pidsMax') && warning.includes('cpuMax')),
    'explicit resource minimum must name all three missing limits')
})

test('resource requires all three limits; partial limits fall back to lifecycle with a warning', async () => {
  const root = await fakeCgroupRoot() // memory, pids, cpu all available
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: {
      mode: 'auto',
      minimumLevel: 'process',
      root,
      stateDir: join(root, 'state'),
      memoryMax: '1073741824',
      pidsMax: '128',
      // cpuMax omitted on purpose
    },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, true)
  assert.equal(report.effectiveLevel, 'lifecycle', 'partial limits must not report resource')
  assert.ok(report.warnings.some((warning) => warning.includes('cpuMax')), 'warning must name the missing limit')
})

test('resource requires all three controllers; a missing controller falls back to lifecycle', async () => {
  const root = await fakeCgroupRoot(['memory', 'pids']) // cpu controller absent
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: {
      mode: 'auto',
      minimumLevel: 'process',
      root,
      stateDir: join(root, 'state'),
      memoryMax: '1073741824',
      pidsMax: '128',
      cpuMax: '200000 100000',
    },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, true)
  assert.equal(report.effectiveLevel, 'lifecycle')
  assert.ok(report.warnings.some((warning) => warning.includes('controllers unavailable: cpu')), 'warning must name the missing controller')
})

test('enabling cgroup warns that delegation writability is advisory, not a Host veto', async () => {
  const root = await fakeCgroupRoot()
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'auto', minimumLevel: 'process', root, stateDir: join(root, 'state') },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, true)
  assert.ok(report.warnings.some((warning) => warning.includes('advisory')), 'enabled cgroup must carry the advisory warning')
})

test('probe does not treat a root without structural v2 files as cgroup-available', async () => {
  const root = await fakeCgroupRoot()
  // Remove one structural file the probe requires to exist and be readable.
  await rm(join(root, 'cgroup.subtree_control'))
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'auto', minimumLevel: 'process', root, stateDir: join(root, 'state') },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, false)
  assert.equal(report.effectiveLevel, 'process')
  assert.ok(report.warnings.some((warning) => warning.includes('cgroup v2 unavailable')))
})

test('required fails closed below minimumLevel but not for minimumLevel=process', async () => {
  const bin = await fakeBinDir(['censorfs', 'censorfs-mounter'])
  // minimumLevel=process is satisfied by the mount-namespace floor alone:
  // required must NOT force cgroup v2.
  const processLevel = await detectRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'required', minimumLevel: 'process' },
  }), { ...process.env, PATH: bin })
  assert.equal(processLevel.cgroupEnabled, false)
  assert.equal(processLevel.effectiveLevel, 'process')

  // minimumLevel=lifecycle without a delegated root fails closed.
  await assert.rejects(() => detectRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'required', minimumLevel: 'lifecycle' },
  }), { ...process.env, PATH: bin }), /requires minimumLevel lifecycle/u)
})

test('required fails closed when the delegated root cannot satisfy resource controllers', async () => {
  const bin = await fakeBinDir(['censorfs', 'censorfs-mounter'])
  const root = await fakeCgroupRoot(['pids']) // memory controller absent
  await assert.rejects(() => detectRunnerEnvironment(baseConfig({
    runnerIsolation: {
      mode: 'required',
      minimumLevel: 'resource',
      root,
      stateDir: join(root, 'state'),
      memoryMax: '1073741824',
    },
  }), { ...process.env, PATH: bin }), /requires minimumLevel resource/u)
})

test('auto downgrades below minimumLevel and reports it as a warning', async () => {
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'auto', minimumLevel: 'resource' },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, false)
  assert.equal(report.effectiveLevel, 'process')
  assert.ok(report.warnings.some((warning) => warning.includes('below minimumLevel resource')), 'auto must report the downgrade')
  assert.ok(report.warnings.some((warning) => warning.includes('cgroup v2 unavailable')))
})

test('process mode never enables cgroup even when a delegated root exists', async () => {
  const root = await fakeCgroupRoot()
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'process', minimumLevel: 'process', root, stateDir: join(root, 'state') },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.cgroupEnabled, false)
  assert.equal(report.effectiveLevel, 'process')
})

test('process mode fails closed when minimumLevel exceeds process', async () => {
  const bin = await fakeBinDir(['censorfs', 'censorfs-mounter'])
  await assert.rejects(() => detectRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'process', minimumLevel: 'lifecycle' },
  }), { ...process.env, PATH: bin }), /requires minimumLevel lifecycle/u)
})

test('external mode delegates isolation without failing closed and reports local process level', async () => {
  const report = await probeRunnerEnvironment(baseConfig({
    runnerIsolation: { mode: 'external', minimumLevel: 'resource' },
  }), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  // external is a deployment statement, not an isolation level: the locally
  // observed level stays process and no internal minimumLevel is enforced.
  assert.equal(report.effectiveLevel, 'process')
  assert.equal(report.cgroupEnabled, false)
  assert.equal(report.error, undefined)
  assert.ok(report.warnings.some((warning) =>
    warning.includes('delegated to an external boundary') && warning.includes('remains process')), 'external must not disguise its boundary as a local isolation level')
})

test('probe keeps the complete report when the policy fails closed; detect throws it', async () => {
  const bin = await fakeBinDir(['censorfs', 'censorfs-mounter'])
  const config = baseConfig({
    runnerIsolation: { mode: 'required', minimumLevel: 'lifecycle' },
  })
  const report = await probeRunnerEnvironment(config, { ...process.env, PATH: bin })
  // The doctor path must retain every diagnostic alongside the failure.
  assert.match(report.error ?? '', /requires minimumLevel lifecycle/u)
  assert.equal(report.effectiveLevel, 'process')
  assert.equal(report.cgroupEnabled, false)
  assert.ok(report.warnings.some((warning) => warning.includes('cgroup v2 unavailable')))
  assert.equal(report.commands.censorfs, join(bin, 'censorfs'))
  assert.equal(typeof report.fuse, 'boolean')
  assert.equal(report.daemon.available, false)
  assert.equal(typeof report.commands.node, 'string')
  await assert.rejects(() => detectRunnerEnvironment(config, { ...process.env, PATH: bin }), /requires minimumLevel lifecycle/u)
})

test('command detection resolves PATH entries and reports missing binaries', async () => {
  const bin = await fakeBinDir(['censorfs', 'censorfs-mounter', 'bwrap'])
  const env = { ...process.env, PATH: bin }
  const report = await probeRunnerEnvironment(baseConfig(), env)
  assert.equal(report.commands.censorfs, join(bin, 'censorfs'))
  assert.equal(report.commands.mounter, join(bin, 'censorfs-mounter'))
  assert.equal(report.commands.bwrap, join(bin, 'bwrap'))
  assert.ok(report.commands.node, 'current node executable must resolve')

  const missing = await probeRunnerEnvironment(baseConfig({ censorfsCommand: 'censorfs' }), { ...process.env, PATH: join(bin, 'empty-dir') })
  assert.equal(missing.commands.censorfs, undefined)
  assert.ok(missing.warnings.some((warning) => warning.includes('censorfs command is not executable')))
})

test('daemon probe reports a missing socket and a non-socket path', async () => {
  const missing = await probeDaemonSocket(join(tmpdir(), 'censorfs-control-missing.sock'))
  assert.equal(missing.available, false)
  assert.match(missing.reason, /not found/u)

  const file = join(tmpdir(), 'censorfs-control-file.sock')
  await writeFile(file, 'not a socket')
  const notSocket = await probeDaemonSocket(file)
  assert.equal(notSocket.available, false)
  assert.match(notSocket.reason, /not a socket/u)
})

test('daemon probe resolves (never rejects) for pathological socket paths', async () => {
  // Overlong paths may fail with platform-specific errors (ENAMETOOLONG on
  // Linux, path-length errors on Windows); whatever the code, the probe must
  // settle with a structured result instead of rejecting the whole probe.
  const overlong = `${join(tmpdir(), 'censorfs-control')}${'x'.repeat(8192)}.sock`
  const longResult = await probeDaemonSocket(overlong)
  assert.equal(longResult.available, false)
  assert.equal(typeof longResult.reason, 'string')
  assert.ok(longResult.reason.length > 0)

  // And the whole environment probe must never throw because of a bad socket.
  const report = await probeRunnerEnvironment(baseConfig({ socket: overlong }), {
    ...process.env,
    PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']),
  })
  assert.equal(report.daemon.available, false)
  // The warning must carry the probe's own failure reason. That reason is
  // path/platform dependent (ENAMETOOLONG on Linux, path-length errors on
  // Windows), so do not assume it contains a literal 'socket' substring.
  const daemonReason = report.daemon.reason
  assert.equal(typeof daemonReason, 'string')
  assert.ok(daemonReason.length > 0)
  assert.ok(report.warnings.some((warning) =>
    warning.includes(daemonReason) || warning.includes(longResult.reason)),
  'warning must carry the daemon failure reason')
})

test('daemon probe connects to a live Unix socket and flags a stale one', { skip: process.platform === 'win32' }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'censorfs-socket-'))
  try {
    const livePath = join(dir, 'live.sock')
    const server = createServer()
    await new Promise((resolve, reject) => {
      server.once('error', reject)
      server.listen(livePath, resolve)
    })
    try {
      const live = await probeDaemonSocket(livePath)
      assert.equal(live.available, true)
      assert.equal(live.live, true)
    } finally {
      await new Promise((resolve) => server.close(resolve))
    }

    // Node's server.close() unlinks the socket path it was bound to, so
    // closing a listener leaves no file behind on Linux. To build a genuine
    // stale Unix socket without external programs, rename the bound inode to
    // a new name first: close() only tries the original path, so the renamed
    // socket file survives on disk with no listener listening on it.
    const stalePath = join(dir, 'stale.sock')
    const staleServer = createServer()
    await new Promise((resolve, reject) => {
      staleServer.once('error', reject)
      staleServer.listen(livePath, resolve)
    })
    await rename(livePath, stalePath)
    await new Promise((resolve) => staleServer.close(resolve))
    const stale = await probeDaemonSocket(stalePath)
    assert.equal(stale.available, true)
    assert.equal(stale.live, false)
    assert.match(stale.reason, /no daemon is listening/u)
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
})

test('probeRunnerEnvironment surfaces daemon and fuse facts without throwing', async () => {
  const report = await probeRunnerEnvironment(baseConfig(), { ...process.env, PATH: await fakeBinDir(['censorfs', 'censorfs-mounter']) })
  assert.equal(report.daemon.available, false)
  assert.equal(typeof report.fuse, 'boolean')
  assert.ok(report.warnings.some((warning) => warning.includes('daemon socket not found')))
})
