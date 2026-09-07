import { randomUUID } from 'node:crypto'
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { mkdir, appendFile } from 'node:fs/promises'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { scrubbedParentEnv } from '@deepseek-ai/dsh-subprocess'
import { temporaryEnvironment } from './environment.js'

const RUNNER_SCRIPT = fileURLToPath(new URL('./runner-process.js', import.meta.url))

function errorText(error) {
  return error instanceof Error ? error.message : String(error)
}

export class RunnerRpcError extends Error {
  constructor(code, message) {
    super(message)
    this.name = 'RunnerRpcError'
    this.code = code
  }
}

export class RunnerClient {
  constructor(child, record, logger) {
    this.child = child
    this.record = record
    this.logger = logger
    this.pending = new Map()
    this.sequence = 0
    this.closed = false
    this.lines = createInterface({ input: child.stdout, crlfDelay: Infinity })
    this.lines.on('line', (line) => this.onLine(line))
    child.once('error', (error) => this.failAll(error))
    child.once('close', (code, signal) => this.failAll(new Error(`CensorFS Runner exited (${code ?? signal ?? 'unknown'})`)))
    // stdin 的 'error' 事件必须有人接：worker 被中断/强制 dispose 后 runner 管道
    // 已断，在途的 write 会 EPIPE —— write 回调能拒掉那一笔，但 'error' 事件
    // 无人监听会把整个 web 进程炸掉。统一汇入 failAll（幂等）。
    child.stdin.on('error', (error) => this.failAll(error))
    child.stderr.on('data', (chunk) => logger?.warn?.(`censorfs-runner ${record.runnerId}: ${chunk.toString('utf8').trimEnd()}`))
  }

  onLine(line) {
    let response
    try {
      response = JSON.parse(line)
    } catch (error) {
      this.failAll(new Error(`CensorFS Runner returned invalid JSON: ${errorText(error)}`))
      return
    }
    const pending = this.pending.get(response.id)
    if (pending === undefined) return
    this.pending.delete(response.id)
    pending.signal?.removeEventListener('abort', pending.onAbort)
    if (response.ok) pending.resolve(response.result)
    else pending.reject(new RunnerRpcError(response.error?.code ?? 'RUNNER_FAILED', response.error?.message ?? 'Runner request failed'))
  }

  call(method, params = {}, signal) {
    if (this.closed) return Promise.reject(new RunnerRpcError('RUNNER_CLOSED', 'Runner is closed'))
    if (signal?.aborted) return Promise.reject(new RunnerRpcError('ABORTED', 'Runner request was aborted'))
    const id = `${this.record.runnerId}:${++this.sequence}`
    return new Promise((resolve, reject) => {
      const onAbort = () => {
        this.pending.delete(id)
        reject(new RunnerRpcError('ABORTED', 'Runner request was aborted'))
        // The MVP protocol is sequential. Killing the runner is the only
        // fail-closed way to guarantee an in-flight process cannot continue.
        this.terminate()
      }
      this.pending.set(id, { resolve, reject, signal, onAbort })
      signal?.addEventListener('abort', onAbort, { once: true })
      this.child.stdin.write(`${JSON.stringify({ id, method, params })}\n`, (error) => {
        if (error === null || error === undefined) return
        this.pending.delete(id)
        signal?.removeEventListener('abort', onAbort)
        reject(error)
      })
    })
  }

  failAll(error) {
    if (this.closed) return
    this.closed = true
    for (const pending of this.pending.values()) {
      pending.signal?.removeEventListener('abort', pending.onAbort)
      pending.reject(error)
    }
    this.pending.clear()
  }

  terminate() {
    if (this.child.exitCode !== null) return
    try {
      if (this.record.cgroupEnabled) this.child.kill('SIGTERM')
      else if (process.platform !== 'win32' && this.child.pid !== undefined) process.kill(-this.child.pid, 'SIGKILL')
      else this.child.kill('SIGKILL')
    } catch (error) {
      if (error.code !== 'ESRCH') throw error
    }
  }

  async dispose() {
    if (!this.closed) {
      let shutdownTimer
      const graceful = await Promise.race([
        this.call('shutdown').then(() => true, () => true),
        new Promise((resolve) => {
          shutdownTimer = setTimeout(resolve, 1000, false)
          shutdownTimer.unref?.()
        }),
      ]).finally(() => clearTimeout(shutdownTimer))
      if (!graceful) this.terminate()
    }
    this.child.stdin.end()
    let closeTimer
    const exited = await Promise.race([
      new Promise((resolve) => this.child.once('close', resolve)),
      new Promise((resolve) => {
        closeTimer = setTimeout(resolve, 1000, 'timeout')
        closeTimer.unref?.()
      }),
    ]).finally(() => clearTimeout(closeTimer))
    if (exited === 'timeout') this.terminate()
    this.lines.close()
    this.failAll(new RunnerRpcError('RUNNER_CLOSED', 'Runner was disposed'))
  }
}

export class RunnerManager {
  constructor(config, logger) {
    this.config = config
    this.logger = logger
    this.byAgent = new WeakMap()
    this.bySession = new Map()
    this.records = new Set()
  }

  async audit(record, event) {
    const directory = this.config.runnerAuditDir
    if (typeof directory !== 'string' || directory.length === 0) return
    const entry = {
      at: Date.now(), event, runnerId: record.runnerId, runId: record.runId,
      variantId: record.variantId, viewId: record.viewId, state: record.state,
      cgroupEnabled: record.cgroupEnabled,
    }
    await mkdir(directory, { recursive: true, mode: 0o700 })
    await appendFile(join(directory, 'runners.jsonl'), `${JSON.stringify(entry)}\n`, { mode: 0o600 })
  }

  get(agent) {
    return agent === undefined ? undefined : this.byAgent.get(agent)
  }

  async launch(binding, signal) {
    if (signal?.aborted) throw new Error('CensorFS Runner launch was cancelled')
    const runnerId = `runner-${randomUUID()}`
    const args = [
      '--socket', this.config.socket,
      '--view-id', binding.viewId,
      '--uid', String(binding.uid),
      '--gid', String(binding.gid),
    ]
    // The cgroup switch is driven by the startup probe result
    // (runnerIsolation.cgroupEnabled), so production runs use exactly what
    // was detected — /censorfs-doctor alone never changes behavior.
    const isolation = this.config.runnerIsolation
    if (isolation.cgroupEnabled === true) {
      if (typeof isolation.root !== 'string' || typeof isolation.stateDir !== 'string') {
        throw new Error('CensorFS Runner cgroup isolation is enabled but runnerIsolation.root/stateDir are not configured')
      }
      args.push(
        '--cgroup-root', isolation.root,
        '--cgroup-state-dir', isolation.stateDir,
        '--cgroup-scope', runnerId,
        '--cgroup-cleanup-timeout-ms', String(isolation.cleanupTimeoutMs),
      )
      if (isolation.memoryMax !== undefined) args.push('--cgroup-memory-max', isolation.memoryMax)
      if (isolation.pidsMax !== undefined) args.push('--cgroup-pids-max', isolation.pidsMax)
      if (isolation.cpuMax !== undefined) args.push('--cgroup-cpu-max', isolation.cpuMax)
      this.logger?.info?.(`censorfs-runner ${runnerId}: cgroup v2 isolation scope=${runnerId} root=${isolation.root} stateDir=${isolation.stateDir} memoryMax=${isolation.memoryMax ?? '-'} pidsMax=${isolation.pidsMax ?? '-'} cpuMax=${isolation.cpuMax ?? '-'}`)
    }
    args.push(
      '--',
      process.execPath,
      RUNNER_SCRIPT,
      '--runner-id', runnerId,
      '--view-id', binding.viewId,
    )
    const child = spawn(this.config.mounterCommand, args, {
      cwd: this.config.controlPlaneCwd,
      env: {
        ...scrubbedParentEnv(),
        ...temporaryEnvironment(binding.tmpDir),
        CENSORFS_RUN_ID: binding.runId,
        CENSORFS_VARIANT_ID: binding.variantId,
        CENSORFS_RUNNER_ID: runnerId,
      },
      detached: process.platform !== 'win32',
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    })
    const record = {
      runnerId,
      viewId: binding.viewId,
      runId: binding.runId,
      variantId: binding.variantId,
      cgroupEnabled: this.config.runnerIsolation.cgroupEnabled === true,
      state: 'creating',
      child,
      client: undefined,
      agent: undefined,
      sessionId: undefined,
      cleanup: undefined,
    }
    const client = new RunnerClient(child, record, this.logger)
    record.client = client
    this.records.add(record)
    try {
      let readinessTimer
      const health = await Promise.race([
        client.call('health', {}, signal),
        new Promise((_, reject) => {
          readinessTimer = setTimeout(() => reject(new Error('CensorFS Runner readiness timed out')), this.config.runnerReadyTimeoutMs)
          readinessTimer.unref?.()
        }),
      ]).finally(() => clearTimeout(readinessTimer))
      if (health.protocolVersion !== 1 || health.runnerId !== runnerId || health.viewId !== binding.viewId || health.cwd !== '/workspace') {
        throw new Error('CensorFS Runner readiness identity mismatch')
      }
      record.state = 'ready'
      void this.audit(record, 'ready').catch((error) => this.logger?.warn?.(`could not audit ${runnerId}: ${errorText(error)}`))
      return record
    } catch (error) {
      await this.disposeRecord(record)
      throw error
    }
  }

  bind(agent, record) {
    if (record.state !== 'ready') throw new Error(`cannot bind Runner in state ${record.state}`)
    if (this.byAgent.has(agent)) throw new Error('Agent already has a CensorFS Runner')
    record.agent = agent
    record.sessionId = agent.session.id
    record.state = 'running'
    this.byAgent.set(agent, record)
    this.bySession.set(agent.session.id, record)
    return () => this.disposeAgent(agent)
  }

  commitBinding(agent, record) {
    if (record.state !== 'running' || record.agent !== agent || this.byAgent.get(agent) !== record
      || record.client.closed || record.child.exitCode !== null) {
      throw new Error('CensorFS Runner became unavailable before Agent publication')
    }
  }

  async disposeAgent(agent) {
    const record = this.byAgent.get(agent)
    if (record === undefined) return
    this.byAgent.delete(agent)
    if (this.bySession.get(record.sessionId) === record) this.bySession.delete(record.sessionId)
    await this.disposeRecord(record)
  }

  async disposeRecord(record) {
    if (record.cleanup !== undefined) return record.cleanup
    record.cleanup = (async () => {
      record.state = 'stopping'
      await record.client.dispose().catch((error) => this.logger?.warn?.(`could not dispose ${record.runnerId}: ${errorText(error)}`))
      record.state = 'stopped'
      await this.audit(record, 'stopped').catch((error) => this.logger?.warn?.(`could not audit ${record.runnerId}: ${errorText(error)}`))
      this.records.delete(record)
    })()
    return record.cleanup
  }

  async dispose() {
    await Promise.allSettled([...this.records].map((record) => this.disposeRecord(record)))
  }
}
