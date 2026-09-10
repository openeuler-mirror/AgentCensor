import { createHash, randomUUID } from 'node:crypto'
import { spawn } from 'node:child_process'

const DEFAULT_MAX_OUTPUT = 4 * 1024 * 1024

export class CensorFsCommandError extends Error {
  constructor(message, { code, censorfsCode, stdout, stderr } = {}) {
    super(message)
    this.name = 'CensorFsCommandError'
    this.code = code
    this.censorfsCode = censorfsCode
    this.stdout = stdout
    this.stderr = stderr
  }

  get stale() {
    return this.censorfsCode === 'HeadChanged' || /HeadChanged|current head is/u.test(this.message)
  }
}

// 幂等请求 ID，避免重试造成重复状态转换
export function derivedRequestId(runId, variantId, phase) {
  const digest = createHash('sha256')
    .update('dsh-branch-explore-v1\0')
    .update(runId)
    .update('\0')
    .update(variantId)
    .update('\0')
    .update(phase)
    .digest()
  const bytes = Buffer.from(digest.subarray(0, 16))
  bytes[6] = (bytes[6] & 0x0f) | 0x50
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = bytes.toString('hex')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

function collect(stream, limit, label) {
  return new Promise((resolve, reject) => {
    const chunks = []
    let size = 0
    stream.on('data', (chunk) => {
      size += chunk.length
      if (size > limit) {
        reject(new CensorFsCommandError(`${label} exceeded ${limit} bytes`))
        stream.destroy()
        return
      }
      chunks.push(chunk)
    })
    stream.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    stream.on('error', reject)
  })
}

export async function runProcess(command, args, options = {}) {
  if (options.signal?.aborted) throw new CensorFsCommandError('process launch was cancelled')
  const child = spawn(command, args, {
    cwd: options.cwd,
    env: options.env,
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  })
  const maxOutput = options.maxOutput ?? DEFAULT_MAX_OUTPUT
  const stdout = collect(child.stdout, maxOutput, 'stdout')
  const stderr = collect(child.stderr, maxOutput, 'stderr')
  let timer
  if (options.timeoutMs !== undefined) {
    timer = setTimeout(() => child.kill('SIGKILL'), options.timeoutMs)
    timer.unref?.()
  }
  const aborted = () => child.kill('SIGTERM')
  options.signal?.addEventListener('abort', aborted, { once: true })
  const code = await new Promise((resolve, reject) => {
    child.once('error', reject)
    child.once('close', resolve)
  }).finally(() => {
    if (timer !== undefined) clearTimeout(timer)
    options.signal?.removeEventListener('abort', aborted)
  })
  const [out, err] = await Promise.all([stdout, stderr])
  return { code, stdout: out, stderr: err }
}

function expectMachineResult(result) {
  let value
  try {
    value = JSON.parse(result.stdout)
  } catch {
    throw new CensorFsCommandError('censorfs returned invalid JSON', result)
  }
  if (result.code !== 0 || value?.ok !== true) {
    throw new CensorFsCommandError(
      value?.error ?? result.stderr.trim() ?? `censorfs exited ${result.code}`,
      { ...result, censorfsCode: value?.code },
    )
  }
  return value.result
}

/**
 * CensorFS CLI 客户端 — FUSE 模式专用。
 *
 * 共享模式（fsEnabled: false）不使用此类。
 * FUSE 模式（fsEnabled: true）通过此类操作 CensorFS daemon。
 */
export class CensorFsCli {
  constructor({ command = 'censorfs', socket, cwd, env = process.env } = {}) {
    if (typeof socket !== 'string' || socket.length === 0) throw new TypeError('CensorFS socket is required')
    this.command = command
    this.socket = socket
    this.cwd = cwd
    this.env = env
  }

  async invoke(subcommand, args, requestId = randomUUID()) {
    const result = await runProcess(this.command, [
      '--socket', this.socket,
      '--request-id', requestId,
      subcommand,
      ...args,
    ], { cwd: this.cwd, env: this.env })
    return expectMachineResult(result)
  }

  async head(branch) {
    const result = await runProcess(this.command, [
      '--socket', this.socket,
      '--json',
      'head', branch,
    ], { cwd: this.cwd, env: this.env })
    if (result.code !== 0) throw new CensorFsCommandError(result.stderr.trim() || 'could not read CensorFS head', result)
    try {
      return JSON.parse(result.stdout)
    } catch {
      throw new CensorFsCommandError('censorfs head returned invalid JSON', result)
    }
  }

  open({ branch, expectedHead, runId, variantId }) {
    return this.invoke('variant-open', [
      '--branch', branch,
      '--expected-generation', expectedHead.generation_id,
      '--expected-head-seq', String(expectedHead.head_seq),
      '--run', runId,
      '--variant', variantId,
    ], derivedRequestId(runId, variantId, 'open'))
  }

  prepare({ ticketId, viewId, runId, variantId, timeoutMs = 30000, maxDiffFileBytes = 262144 }) {
    return this.invoke('variant-prepare', [
      '--ticket', ticketId,
      ...(viewId === undefined ? [] : ['--view', viewId]),
      '--run', runId,
      '--variant', variantId,
      '--timeout-ms', String(timeoutMs),
      '--max-diff-file-bytes', String(maxDiffFileBytes),
    ], derivedRequestId(runId, variantId, 'prepare'))
  }

  publish({ candidateId, expectedHead, decisionId, runId, variantId }) {
    return this.invoke('variant-publish', [
      '--candidate', candidateId,
      '--expected-generation', expectedHead.generation_id,
      '--expected-head-seq', String(expectedHead.head_seq),
      '--decision-id', decisionId,
      '--run', runId,
      '--variant', variantId,
    ], derivedRequestId(runId, variantId, `publish:${decisionId}`))
  }

  abort({ ticketId, viewId, runId, variantId }) {
    return this.invoke('variant-abort', [
      '--ticket', ticketId,
      ...(viewId === undefined ? [] : ['--view', viewId]),
      '--run', runId,
      '--variant', variantId,
    ], derivedRequestId(runId, variantId, 'abort'))
  }

  openCandidate(candidateId) {
    return this.invoke('candidate-view-open', ['--candidate', candidateId])
  }

  openGeneration(generationId) {
    return this.invoke('generation-view-open', ['--generation', generationId])
  }

  closeView(viewId) {
    return this.invoke('view-close', ['--view', viewId])
  }
}