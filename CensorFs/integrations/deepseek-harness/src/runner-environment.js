import { access, readFile, stat } from 'node:fs/promises'
import { constants } from 'node:fs'
import { connect } from 'node:net'
import { delimiter, isAbsolute, join } from 'node:path'

// Isolation levels form a strict order. process is the floor provided by the
// CensorFS mount namespace itself (mounter) plus bubblewrap sandboxing;
// lifecycle adds a delegated cgroup v2 scope; resource adds cgroup resource
// controllers on top of that scope.
const LEVELS = { process: 0, lifecycle: 1, resource: 2 }

function errorText(error) {
  return error instanceof Error ? error.message : String(error)
}

async function executable(command, env = process.env) {
  if (typeof command !== 'string' || command.length === 0) return undefined
  const candidates = isAbsolute(command) || command.includes('/')
    ? [command]
    : (env.PATH ?? '').split(delimiter).filter(Boolean).map((entry) => join(entry, command))
  for (const candidate of candidates) {
    try {
      await access(candidate, constants.X_OK)
      return candidate
    } catch {}
  }
  return undefined
}

// Read-only daemon liveness probe: connect() to the existing Unix socket and
// immediately close. This never starts a daemon and never modifies anything;
// it only distinguishes a live daemon from a stale socket file left behind by
// a dead one.
export async function probeDaemonSocket(socketPath) {
  if (typeof socketPath !== 'string' || socketPath.length === 0) {
    return { available: false, reason: 'no daemon socket configured' }
  }
  let info
  try {
    info = await stat(socketPath)
  } catch (error) {
    if (error?.code === 'ENOENT') {
      return { available: false, socket: socketPath, reason: `daemon socket not found: ${socketPath}` }
    }
    return { available: false, socket: socketPath, reason: errorText(error) }
  }
  if (!info.isSocket()) {
    return { available: false, socket: socketPath, reason: `socket path exists but is not a socket: ${socketPath}` }
  }
  // A connect failure must never reject the probe: the file-level facts
  // (exists, is a socket) are already established, so any synchronous or
  // asynchronous connect exception is reported as not-live with a reason.
  // Real writability/liveness authority stays with the mounter/daemon
  // handshake; this probe only classifies the socket.
  let live = false
  try {
    live = await new Promise((resolve) => {
      let timer
      const settle = (value) => {
        clearTimeout(timer)
        resolve(value)
      }
      let client
      try {
        client = connect(socketPath)
      } catch (error) {
        settle(false)
        return
      }
      timer = setTimeout(() => { client.destroy(); settle(false) }, 1000)
      timer.unref?.()
      client.once('connect', () => { client.destroy(); settle(true) })
      client.once('error', () => { client.destroy(); settle(false) })
    })
  } catch (error) {
    return { available: true, socket: socketPath, live: false, reason: `socket connect failed: ${errorText(error)}` }
  }
  return {
    available: true,
    socket: socketPath,
    live,
    ...(live ? {} : { reason: `socket exists but no daemon is listening: ${socketPath}` }),
  }
}

// Read-only inspection of a candidate delegated cgroup v2 root. This probe
// never writes: it verifies the root is a directory and that the v2 structural
// files (cgroup.controllers, cgroup.procs, cgroup.type, cgroup.events,
// cgroup.subtree_control) exist and are readable, proving this is a real v2
// subtree rather than an ordinary directory.
//
// Deliberately NOT checked: write access. The Harness Host process (usually
// non-root) may legitimately be unable to write a subtree that the privileged
// mounter can; Host W_OK must never veto a delegated root. Real writability is
// decided by the mounter launch/readiness handshake (required fails closed,
// auto falls back to process isolation). The report therefore carries
// advisory: true.
async function cgroupCapability(config) {
  if (config.root === undefined) {
    return { available: false, level: 'process', controllers: [], reason: 'no delegated cgroup v2 root configured' }
  }
  try {
    const metadata = await stat(config.root)
    if (!metadata.isDirectory()) throw new Error('root is not a directory')
    for (const structural of ['cgroup.controllers', 'cgroup.procs', 'cgroup.type', 'cgroup.events', 'cgroup.subtree_control']) {
      await access(join(config.root, structural), constants.R_OK)
    }
    const controllers = (await readFile(join(config.root, 'cgroup.controllers'), 'utf8')).trim().split(/\s+/u).filter(Boolean)
    // resource level per the product contract: memory/pids/cpu ALL THREE must
    // be configured AND all three controllers must be available. Partial
    // limits (or a missing controller) fall back to lifecycle and are listed
    // for the warning, so a partial setup is never over-reported as resource.
    const configuredLimits = {
      memory: config.memoryMax !== undefined,
      pids: config.pidsMax !== undefined,
      cpu: config.cpuMax !== undefined,
    }
    const missingRequestedControllers = ['memory', 'pids', 'cpu']
      .filter((controller) => configuredLimits[controller] && !controllers.includes(controller))
    const resourceReady = configuredLimits.memory && configuredLimits.pids && configuredLimits.cpu
      && missingRequestedControllers.length === 0
    const missingResourceLimits = resourceReady
      ? []
      : ['memory', 'pids', 'cpu'].filter((controller) => !configuredLimits[controller])
    return {
      available: true,
      level: resourceReady ? 'resource' : 'lifecycle',
      controllers: ['memory', 'pids', 'cpu'].filter((controller) => controllers.includes(controller)),
      missingRequestedControllers,
      missingResourceLimits,
      root: config.root,
      advisory: true,
    }
  } catch (error) {
    return { available: false, level: 'process', controllers: [], root: config.root, reason: errorText(error) }
  }
}

/**
 * Normalize runnerIsolation config.
 *
 * Modes:
 * - auto:     probe cgroup v2; use it when available, otherwise downgrade to
 *             process isolation. Never throws; reports every downgrade as a
 *             warning, including falling below `minimumLevel`.
 * - required: fail closed whenever the effective level is below
 *             `minimumLevel`. With minimumLevel=process this is satisfied by
 *             the mount-namespace floor alone — cgroup v2 is NOT forced.
 * - process:  never use cgroup v2; process isolation only. minimumLevel above
 *             process fails closed because the mode cannot provide it.
 * - external: isolation is delegated to the external CensorFS/mounter
 *             boundary; this plugin manages no cgroup v2 and does not enforce
 *             minimumLevel.
 *
 * The legacy `runnerCgroup` block keeps working: when `runnerIsolation` omits
 * a field, the legacy value fills the gap, and `runnerCgroup.enabled === true`
 * defaults mode to `required` and minimumLevel to `lifecycle`.
 */
export function normalizeIsolationConfig(value = {}, legacy = {}) {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new TypeError('censorfs-parallel-worlds runnerIsolation must be an object')
  }
  const legacyEnabled = legacy.enabled === true
  const mode = value.mode ?? (legacyEnabled ? 'required' : 'auto')
  const minimumLevel = value.minimumLevel ?? (legacyEnabled ? 'lifecycle' : 'process')
  if (!['auto', 'required', 'process', 'external'].includes(mode)) {
    throw new TypeError('censorfs-parallel-worlds runnerIsolation.mode must be auto, required, process, or external')
  }
  if (!Object.hasOwn(LEVELS, minimumLevel)) {
    throw new TypeError('censorfs-parallel-worlds runnerIsolation.minimumLevel must be process, lifecycle, or resource')
  }
  const config = {
    mode,
    minimumLevel,
    root: value.root ?? legacy.root,
    stateDir: value.stateDir ?? legacy.stateDir,
    memoryMax: value.memoryMax ?? legacy.memoryMax,
    pidsMax: value.pidsMax ?? legacy.pidsMax,
    cpuMax: value.cpuMax ?? legacy.cpuMax,
    cleanupTimeoutMs: value.cleanupTimeoutMs ?? legacy.cleanupTimeoutMs ?? 5000,
  }
  for (const key of ['root', 'stateDir', 'memoryMax', 'pidsMax', 'cpuMax']) {
    if (config[key] !== undefined && (typeof config[key] !== 'string' || config[key].length === 0)) {
      throw new TypeError(`censorfs-parallel-worlds runnerIsolation.${key} must be a non-empty string`)
    }
  }
  if (!Number.isSafeInteger(config.cleanupTimeoutMs) || config.cleanupTimeoutMs <= 0) {
    throw new TypeError('censorfs-parallel-worlds runnerIsolation.cleanupTimeoutMs must be a positive safe integer')
  }
  if (config.root !== undefined && config.stateDir === undefined) {
    throw new TypeError('censorfs-parallel-worlds runnerIsolation.stateDir is required with root')
  }
  return config
}

/**
 * Probe the full runner environment without throwing. Read-only: it stats and
 * connects only, never starts a daemon, writes sudoers, mounts cgroup v2, or
 * otherwise modifies the system. When the isolation policy fails closed, the
 * returned report carries an `error` field and every other detail for
 * diagnosis (used by /censorfs-doctor).
 */
export async function probeRunnerEnvironment(config, env = process.env) {
  const isolation = config.runnerIsolation
  const [daemon, censorfs, mounter, node, bwrap, fuse, cgroup] = await Promise.all([
    probeDaemonSocket(config.socket),
    executable(config.censorfsCommand, env),
    executable(config.mounterCommand, env),
    executable(process.execPath, env),
    executable('bwrap', env),
    stat('/dev/fuse').then((value) => value.isCharacterDevice(), () => false),
    cgroupCapability(isolation),
  ])
  const warnings = []
  let effectiveLevel = 'process'
  let cgroupEnabled = false
  if (isolation.mode === 'external') {
    warnings.push('Runner isolation is delegated to an external boundary; locally observed isolation remains process')
  } else if (isolation.mode !== 'process' && cgroup.available) {
    effectiveLevel = cgroup.level
    cgroupEnabled = true
    warnings.push('delegation writability is advisory; mounter readiness is authoritative')
  } else if (isolation.mode !== 'process') {
    warnings.push(`cgroup v2 unavailable: ${cgroup.reason}`)
  }
  if (cgroup.missingRequestedControllers?.length > 0) {
    warnings.push(`requested cgroup controllers unavailable: ${cgroup.missingRequestedControllers.join(', ')}`)
  }
  // Only report missing resource limits when they are actually relevant:
  // resource was explicitly requested (minimumLevel=resource), or at least one
  // limit is configured but the set is incomplete. A plain lifecycle setup
  // with NO limits configured must not spam "missing all limits".
  const missingLimits = cgroup.missingResourceLimits ?? []
  const anyLimitConfigured = missingLimits.length < 3
  if (missingLimits.length > 0 && (isolation.minimumLevel === 'resource' || anyLimitConfigured)) {
    warnings.push(`resource isolation requires memoryMax, pidsMax, and cpuMax; missing: ${missingLimits.map((controller) => `${controller}Max`).join(', ')}`)
  }
  const belowMinimum = LEVELS[effectiveLevel] < LEVELS[isolation.minimumLevel]
  let error
  if (belowMinimum && isolation.mode !== 'external') {
    if (isolation.mode === 'auto') {
      warnings.push(`effective isolation ${effectiveLevel} is below minimumLevel ${isolation.minimumLevel}; auto mode continues at ${effectiveLevel}`)
    } else {
      error = `Runner isolation mode ${isolation.mode} requires minimumLevel ${isolation.minimumLevel}, but only ${effectiveLevel} is available`
    }
  }
  if (censorfs === undefined) warnings.push(`censorfs command is not executable: ${config.censorfsCommand}`)
  if (mounter === undefined) warnings.push(`mounter command is not executable: ${config.mounterCommand}`)
  if (daemon.available !== true) warnings.push(daemon.reason)
  else if (daemon.live !== true) warnings.push(daemon.reason)
  if (!fuse) warnings.push('/dev/fuse is unavailable')
  if (bwrap === undefined) warnings.push('bwrap is unavailable; runner bash tools will fail closed')
  return {
    requestedMode: isolation.mode,
    minimumLevel: isolation.minimumLevel,
    effectiveLevel,
    cgroupEnabled,
    controllers: cgroup.controllers ?? [],
    ...(cgroupEnabled ? { cgroupRoot: cgroup.root } : {}),
    daemon,
    commands: { censorfs, mounter, node, bwrap },
    fuse,
    warnings,
    ...(error === undefined ? {} : { error }),
  }
}

/**
 * Probe and enforce the isolation policy. Throws when the mode fails closed
 * (required/process below minimumLevel); the caller either refuses to start
 * or reports the error so exploration can fail closed before any Runner is
 * launched.
 */
export async function detectRunnerEnvironment(config, env = process.env) {
  const report = await probeRunnerEnvironment(config, env)
  if (report.error !== undefined) throw new Error(report.error)
  return report
}
