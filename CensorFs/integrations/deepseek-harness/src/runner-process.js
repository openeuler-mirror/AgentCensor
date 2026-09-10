#!/usr/bin/env node
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { lstat, mkdir, readFile, realpath, readdir, rename, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, isAbsolute, join, matchesGlob, relative, resolve, sep } from 'node:path'
import { randomUUID } from 'node:crypto'

const MAX_FILE_BYTES = 16 * 1024 * 1024
const MAX_OUTPUT_BYTES = 4 * 1024 * 1024

const SANDBOX_ENV_KEYS = [
  'PATH', 'HOME', 'USER', 'LOGNAME', 'LANG', 'LC_ALL', 'TERM', 'TZ', 'TMPDIR',
  'CENSORFS_RUN_ID', 'CENSORFS_VARIANT_ID', 'CENSORFS_RUNNER_ID',
]

export function sandboxEnvironment(parentEnv = process.env) {
  const environment = {}
  for (const key of SANDBOX_ENV_KEYS) {
    const value = parentEnv[key]
    if (value !== undefined) environment[key] = value
  }
  return environment
}

function parseArgs(argv) {
  const values = { workspace: '/workspace' }
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index]
    const value = argv[index + 1]
    if (value === undefined) throw new Error(`missing value for ${key}`)
    if (key === '--runner-id') values.runnerId = value
    else if (key === '--view-id') values.viewId = value
    else if (key === '--workspace') values.workspace = value
    else throw new Error(`unknown argument ${key}`)
  }
  if (!values.runnerId || !values.viewId) throw new Error('--runner-id and --view-id are required')
  return values
}

class RpcError extends Error {
  constructor(code, message) {
    super(message)
    this.code = code
  }
}

function displayPath(raw) {
  if (isAbsolute(raw)) return raw
  return raw === '.' ? '/workspace' : `/workspace/${raw.replaceAll('\\', '/')}`
}

function imageType(data) {
  if (data.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) return 'image/png'
  if (data[0] === 255 && data[1] === 216 && data[2] === 255) return 'image/jpeg'
  if (data.subarray(0, 6).toString('ascii') === 'GIF87a' || data.subarray(0, 6).toString('ascii') === 'GIF89a') return 'image/gif'
  if (data.subarray(0, 4).toString('ascii') === 'RIFF' && data.subarray(8, 12).toString('ascii') === 'WEBP') return 'image/webp'
  return undefined
}

function collect(stream, limit) {
  return new Promise((resolveValue, reject) => {
    const chunks = []
    let retainedBytes = 0
    let totalBytes = 0
    stream.on('data', (chunk) => {
      totalBytes += chunk.length
      chunks.push(chunk)
      retainedBytes += chunk.length
      while (retainedBytes > limit && chunks.length > 0) {
        const excess = retainedBytes - limit
        if (chunks[0].length <= excess) retainedBytes -= chunks.shift().length
        else {
          chunks[0] = chunks[0].subarray(excess)
          retainedBytes -= excess
        }
      }
    })
    stream.on('end', () => resolveValue({ text: Buffer.concat(chunks).toString('utf8'), truncated: totalBytes > limit }))
    stream.on('error', reject)
  })
}

export class NamespaceRunner {
  constructor({ runnerId, viewId, workspace }) {
    this.runnerId = runnerId
    this.viewId = viewId
    this.workspace = resolve(workspace)
    this.quiescing = false
    this.active = new Map()
    this.jobs = new Map()
    this.jobSequence = 0
  }

  async initialize() {
    this.workspace = await realpath(this.workspace)
    const info = await stat(this.workspace)
    if (!info.isDirectory()) throw new RpcError('WORKSPACE_UNAVAILABLE', 'workspace is not a directory')
  }

  async resolvePath(raw, { allowMissing = false } = {}) {
    if (typeof raw !== 'string' || raw.length === 0 || raw.includes('\0')) throw new RpcError('INVALID_REQUEST', 'path must be a non-empty string')
    let relativePath
    if (isAbsolute(raw)) {
      if (raw !== '/workspace' && !raw.startsWith('/workspace/')) throw new RpcError('PATH_OUTSIDE_WORKSPACE', raw)
      relativePath = raw === '/workspace' ? '' : raw.slice('/workspace/'.length)
    } else {
      relativePath = raw
    }
    const target = resolve(this.workspace, relativePath)
    if (target !== this.workspace && !target.startsWith(`${this.workspace}${sep}`)) throw new RpcError('PATH_OUTSIDE_WORKSPACE', raw)
    let existing = allowMissing ? dirname(target) : target
    if (allowMissing) {
      while (existing !== this.workspace) {
        try {
          await stat(existing)
          break
        } catch (error) {
          if (error.code !== 'ENOENT') throw error
          existing = dirname(existing)
        }
      }
    }
    const canonical = await realpath(existing).catch((error) => {
      if (error.code === 'ENOENT') throw new RpcError('NOT_FOUND', raw)
      throw error
    })
    if (canonical !== this.workspace && !canonical.startsWith(`${this.workspace}${sep}`)) throw new RpcError('PATH_OUTSIDE_WORKSPACE', raw)
    if (allowMissing) {
      const leaf = await lstat(target).catch((error) => {
        if (error.code === 'ENOENT') return undefined
        throw error
      })
      if (leaf?.isSymbolicLink()) throw new RpcError('PATH_OUTSIDE_WORKSPACE', 'symbolic-link write targets are not allowed')
    }
    return target
  }

  async readLimited(path) {
    const info = await stat(path)
    if (!info.isFile()) throw new RpcError('NOT_FILE', path)
    if (info.size > MAX_FILE_BYTES) throw new RpcError('FILE_TOO_LARGE', `${path} exceeds ${MAX_FILE_BYTES} bytes`)
    return readFile(path)
  }

  async dispatch(method, params = {}, requestId) {
    if (this.quiescing && method !== 'health' && method !== 'shutdown') throw new RpcError('QUIESCING', 'runner is quiescing')
    if (method === 'health') return { protocolVersion: 1, runnerId: this.runnerId, viewId: this.viewId, cwd: '/workspace', quiescing: this.quiescing }
    if (method === 'quiesce') { this.quiescing = true; return { quiescing: true } }
    if (method === 'shutdown') { this.quiescing = true; return { shutdown: true } }
    if (method === 'fs.read') return this.readText(params)
    if (method === 'fs.write') return this.writeText(params)
    if (method === 'fs.edit') return this.editText(params)
    if (method === 'fs.glob') return this.glob(params)
    if (method === 'fs.grep') return this.grep(params)
    if (method === 'fs.read_image') return this.readImage(params)
    if (method === 'process.run') return this.runProcess(params, requestId)
    if (method === 'job.list') return this.listJobs()
    if (method === 'job.output') return this.outputJob(params)
    if (method === 'job.kill') return this.killJob(params)
    throw new RpcError('METHOD_NOT_FOUND', `unknown method ${method}`)
  }

  async readText(params) {
    const path = await this.resolvePath(params.file_path)
    const text = (await this.readLimited(path)).toString('utf8')
    const offset = params.offset ?? 1
    const limit = params.limit ?? 2000
    if (!Number.isSafeInteger(offset) || offset < 1 || !Number.isSafeInteger(limit) || limit < 1 || limit > 2000) throw new RpcError('INVALID_REQUEST', 'offset and limit must be positive integers; limit must be <= 2000')
    const all = text.split(/\r?\n/u)
    if (all.at(-1) === '') all.pop()
    if (offset > all.length && all.length > 0) throw new RpcError('NOT_FOUND', `offset ${offset} exceeds ${all.length} lines`)
    return {
      path: displayPath(params.file_path),
      offset,
      lines: all.slice(offset - 1, offset - 1 + limit).map((line, index) => ({ number: offset + index, text: line })),
      totalLines: all.length,
    }
  }

  async atomicWrite(path, data) {
    await mkdir(dirname(path), { recursive: true })
    const temp = join(dirname(path), `.censorfs-runner-${process.pid}-${randomUUID()}.tmp`)
    await writeFile(temp, data)
    await rename(temp, path)
  }

  async writeText(params) {
    if (typeof params.content !== 'string') throw new RpcError('INVALID_REQUEST', 'content must be a string')
    if (Buffer.byteLength(params.content) > MAX_FILE_BYTES) throw new RpcError('FILE_TOO_LARGE', params.file_path)
    const path = await this.resolvePath(params.file_path, { allowMissing: true })
    const before = await this.readLimited(path).then((data) => data.toString('utf8')).catch((error) => {
      if (error.code === 'ENOENT' || error.code === 'NOT_FOUND') return null
      throw error
    })
    await this.atomicWrite(path, params.content)
    return { path: displayPath(params.file_path), operation: before === null ? 'create' : 'update', before, after: params.content }
  }

  async editText(params) {
    if (typeof params.old_string !== 'string' || params.old_string.length === 0) throw new RpcError('INVALID_REQUEST', 'old_string must be non-empty')
    if (typeof params.new_string !== 'string' || params.old_string === params.new_string) throw new RpcError('INVALID_REQUEST', 'old_string and new_string must differ')
    const path = await this.resolvePath(params.file_path)
    const before = (await this.readLimited(path)).toString('utf8')
    const count = before.split(params.old_string).length - 1
    if (count === 0) throw new RpcError('EDIT_NO_MATCH', 'old_string was not found')
    if (params.replace_all !== true && count !== 1) throw new RpcError('EDIT_NOT_UNIQUE', `old_string matched ${count} times`)
    const after = params.replace_all === true
      ? before.split(params.old_string).join(params.new_string)
      : before.replace(params.old_string, params.new_string)
    await this.atomicWrite(path, after)
    return { path: displayPath(params.file_path), before, after }
  }

  async listFiles(root) {
    const info = await stat(root)
    if (info.isFile()) return [root]
    if (!info.isDirectory()) throw new RpcError('NOT_FILE', root)
    const files = []
    const pending = [root]
    while (pending.length > 0) {
      const directory = pending.pop()
      for (const entry of await readdir(directory, { withFileTypes: true })) {
        if (entry.name === '.git' || entry.isSymbolicLink()) continue
        const path = join(directory, entry.name)
        if (entry.isDirectory()) pending.push(path)
        else if (entry.isFile()) files.push(path)
      }
    }
    return files
  }

  async glob(params) {
    if (typeof params.pattern !== 'string' || params.pattern.length === 0) throw new RpcError('INVALID_REQUEST', 'pattern must be non-empty')
    const rawRoot = params.path ?? '/workspace'
    const root = await this.resolvePath(rawRoot)
    const rootPrefix = relative(this.workspace, root)
    const paths = (await Promise.all((await this.listFiles(root)).map(async (path) => ({
      path: relative(root, path).replaceAll('\\', '/'),
      modified: (await stat(path)).mtimeMs,
    }))))
      .filter((entry) => matchesGlob(entry.path, params.pattern) || matchesGlob(basename(entry.path), params.pattern))
      .sort((left, right) => right.modified - left.modified || left.path.localeCompare(right.path))
      .map((entry) => join(rootPrefix, entry.path).replaceAll('\\', '/'))
    return { root: displayPath(rawRoot), paths }
  }

  async grep(params) {
    if (typeof params.pattern !== 'string' || params.pattern.length === 0) throw new RpcError('INVALID_REQUEST', 'pattern must be non-empty')
    if (params.include !== undefined && (typeof params.include !== 'string' || params.include.length === 0 || params.include.startsWith('!'))) throw new RpcError('INVALID_REQUEST', 'include must be one positive glob')
    let expression
    try {
      expression = new RegExp(params.pattern, 'u')
    } catch (error) {
      throw new RpcError('INVALID_REQUEST', `invalid regular expression: ${error.message}`)
    }
    const target = await this.resolvePath(params.path ?? '/workspace')
    const matches = []
    for (const path of await this.listFiles(target)) {
      const workspacePath = relative(this.workspace, path).replaceAll('\\', '/')
      if (params.include !== undefined && !matchesGlob(workspacePath, params.include) && !matchesGlob(basename(path), params.include)) continue
      let text
      try {
        text = (await this.readLimited(path)).toString('utf8')
      } catch (error) {
        if (error.code === 'FILE_TOO_LARGE' || error.code === 'NOT_FILE') continue
        throw error
      }
      const lines = text.split(/\r?\n/u)
      for (let index = 0; index < lines.length; index += 1) {
        if (!expression.test(lines[index])) continue
        matches.push({ path: workspacePath, lineNumber: index + 1, line: lines[index] })
        if (matches.length > 10000) throw new RpcError('OUTPUT_TOO_LARGE', 'grep produced more than 10000 matches; narrow the query')
      }
    }
    return { matches }
  }

  async readImage(params) {
    const path = await this.resolvePath(params.file_path)
    const data = await this.readLimited(path)
    const mediaType = imageType(data)
    if (mediaType === undefined) throw new RpcError('UNSUPPORTED_IMAGE', 'expected PNG/JPEG/WebP/GIF bytes')
    return { path: displayPath(params.file_path), mediaType, bytes: data.length, dataBase64: data.toString('base64'), name: basename(path) }
  }

  async sandboxCommand(params) {
    if (params.sandbox_permissions !== undefined || params.justification !== undefined) throw new RpcError('APPROVAL_UNAVAILABLE', 'sandbox escalation is unavailable for delegated CensorFS agents')
    if (typeof params.command !== 'string' || params.command.length === 0) throw new RpcError('INVALID_REQUEST', 'command must be non-empty')
    const workdir = await this.resolvePath(params.workdir ?? '/workspace')
    const sandboxCwd = displayPath(relative(this.workspace, workdir))
    const sandboxArgs = ['--die-with-parent', '--new-session', '--unshare-all', '--share-net']
    for (const systemPath of ['/usr', '/bin', '/lib', '/lib64', '/etc', '/opt']) {
      try {
        await stat(systemPath)
        sandboxArgs.push('--ro-bind', systemPath, systemPath)
      } catch (error) {
        if (error.code !== 'ENOENT') throw error
      }
    }
    sandboxArgs.push('--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--bind', this.workspace, '/workspace', '--chdir', sandboxCwd, '--setenv', 'HOME', '/tmp', '--info-fd', '3', '--', 'bash', '-lc', params.command)
    return { sandboxArgs, workdir }
  }

  spawnSandbox(sandboxArgs) {
    const child = spawn('bwrap', sandboxArgs, {
      cwd: this.workspace,
      env: sandboxEnvironment(),
      detached: false,
      stdio: ['ignore', 'pipe', 'pipe', 'pipe'],
    })
    const infoReady = new Promise((resolvePid, reject) => {
      let info = ''
      let resolved = false
      const parseInfo = () => {
        try {
          const pid = JSON.parse(info)['child-pid']
          if (!Number.isSafeInteger(pid) || pid <= 0) throw new Error('bubblewrap did not return a valid child-pid')
          resolved = true
          resolvePid(pid)
          return true
        } catch {
          return false
        }
      }
      child.stdio[3].setEncoding('utf8')
      child.stdio[3].on('data', (chunk) => {
        info += chunk
        if (info.length > 65536) reject(new Error('bubblewrap info exceeded 64 KiB'))
        else parseInfo()
      })
      child.stdio[3].once('error', reject)
      child.stdio[3].once('end', () => {
        if (!resolved && !parseInfo()) reject(new Error('bubblewrap did not return valid info JSON'))
      })
    })
    child.sandboxReady = Promise.race([
      infoReady.catch(() => undefined),
      new Promise((resolvePid) => {
        const timer = setTimeout(() => resolvePid(undefined), 1000)
        timer.unref?.()
      }),
    ])
    return child
  }

  async runProcess(params, requestId) {
    const { sandboxArgs } = await this.sandboxCommand(params)
    if (params.run_in_background === true) return this.startJob(params, sandboxArgs)
    const requestedTimeout = params.timeoutMs ?? 120000
    if (!Number.isSafeInteger(requestedTimeout) || requestedTimeout <= 0) throw new RpcError('INVALID_REQUEST', 'timeoutMs must be a positive integer')
    const timeoutMs = Math.min(requestedTimeout, 600000)
    const child = this.spawnSandbox(sandboxArgs)
    this.active.set(requestId, child)
    const stdout = collect(child.stdout, MAX_OUTPUT_BYTES)
    const stderr = collect(child.stderr, MAX_OUTPUT_BYTES)
    let timedOut = false
    const timer = setTimeout(() => {
      timedOut = true
      this.killProcess(child)
    }, timeoutMs)
    timer.unref?.()
    const exit = await new Promise((resolveExit, reject) => {
      child.once('error', reject)
      child.once('close', (code, signal) => resolveExit({ code, signal }))
    }).finally(() => {
      clearTimeout(timer)
      this.active.delete(requestId)
    })
    const [out, err] = await Promise.all([stdout, stderr])
    return { kind: 'foreground', exitCode: exit.code, signal: exit.signal, timedOut, aborted: false, timeoutMs, stdout: out, stderr: err }
  }

  publicJob(job) {
    return {
      id: job.id,
      kind: 'bash',
      label: job.label,
      status: job.status,
      ...(job.detail === undefined ? {} : { detail: job.detail }),
      startedAt: job.startedAt,
      ...(job.finishedAt === undefined ? {} : { finishedAt: job.finishedAt }),
    }
  }

  appendJobOutput(job, chunk) {
    job.output += chunk.toString('utf8')
    const bytes = Buffer.byteLength(job.output)
    if (bytes <= MAX_OUTPUT_BYTES) return
    const retained = Buffer.from(job.output).subarray(bytes - MAX_OUTPUT_BYTES).toString('utf8')
    const removed = job.output.length - retained.length
    job.output = retained
    job.cursor = Math.max(0, job.cursor - removed)
    job.truncated = true
  }

  startJob(params, sandboxArgs) {
    const id = `${this.runnerId}:job:${++this.jobSequence}`
    const child = this.spawnSandbox(sandboxArgs)
    const job = {
      id,
      label: params.command,
      child,
      status: 'running',
      startedAt: Date.now(),
      finishedAt: undefined,
      detail: undefined,
      output: '',
      cursor: 0,
      truncated: false,
      killed: false,
      settled: undefined,
    }
    job.settled = new Promise((resolveSettled) => {
      child.stdout.on('data', (chunk) => this.appendJobOutput(job, chunk))
      child.stderr.on('data', (chunk) => this.appendJobOutput(job, chunk))
      child.once('error', (error) => {
        job.status = 'failed'
        job.detail = error.message
        job.finishedAt = Date.now()
        resolveSettled()
      })
      child.once('close', (code, signal) => {
        if (job.finishedAt !== undefined) return
        job.status = job.killed ? 'killed' : code === 0 ? 'completed' : 'failed'
        job.detail = signal === null ? `exit code: ${code ?? 0}` : `signal: ${signal}`
        job.finishedAt = Date.now()
        resolveSettled()
      })
    })
    this.jobs.set(id, job)
    return { kind: 'background', jobId: id }
  }

  requireJob(id) {
    if (typeof id !== 'string' || id.length === 0) throw new RpcError('INVALID_REQUEST', 'job_id must be a non-empty string')
    const job = this.jobs.get(id)
    if (job === undefined) throw new RpcError('NOT_FOUND', `unknown job ${id}`)
    return job
  }

  listJobs() {
    return [...this.jobs.values()].map((job) => this.publicJob(job))
  }

  async outputJob(params) {
    const job = this.requireJob(params.job_id)
    if (params.wait === true && (job.status === 'running' || job.status === 'stopping')) {
      const requested = params.timeout_ms ?? 30000
      if (!Number.isSafeInteger(requested) || requested <= 0) throw new RpcError('INVALID_REQUEST', 'timeout_ms must be a positive integer')
      let timer
      await Promise.race([
        job.settled,
        new Promise((resolveWait) => {
          timer = setTimeout(resolveWait, Math.min(requested, 600000))
          timer.unref?.()
        }),
      ]).finally(() => clearTimeout(timer))
    }
    let text = job.output.slice(job.cursor)
    job.cursor = job.output.length
    if (job.truncated) {
      text = `[output truncated]\n${text}`
      job.truncated = false
    }
    return { text, job: this.publicJob(job) }
  }

  killJob(params) {
    const job = this.requireJob(params.job_id)
    if (job.status !== 'running' && job.status !== 'stopping') {
      return { outcome: 'already-finished', job: this.publicJob(job) }
    }
    job.status = 'stopping'
    job.killed = true
    this.killProcess(job.child)
    return { outcome: 'cancellation-requested', job: this.publicJob(job) }
  }

  killProcess(child) {
    child.sandboxReady.then((sandboxPid) => {
      if (sandboxPid !== undefined && process.platform !== 'win32') {
        try {
          process.kill(-sandboxPid, 'SIGKILL')
        } catch {
          try { process.kill(sandboxPid, 'SIGKILL') } catch {}
        }
      }
      if (child.exitCode === null) child.kill('SIGKILL')
    })
  }

  dispose() {
    this.quiescing = true
    for (const child of this.active.values()) this.killProcess(child)
    for (const job of this.jobs.values()) {
      if (job.status === 'running' || job.status === 'stopping') {
        job.killed = true
        this.killProcess(job.child)
      }
    }
  }
}

export async function runStdio(argv = process.argv.slice(2)) {
  const runner = new NamespaceRunner(parseArgs(argv))
  await runner.initialize()
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
  let chain = Promise.resolve()
  const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`)
  input.on('line', (line) => {
    chain = chain.then(async () => {
      let request
      try {
        request = JSON.parse(line)
        const result = await runner.dispatch(request.method, request.params, request.id)
        send({ id: request.id, ok: true, result })
        if (request.method === 'shutdown') input.close()
      } catch (error) {
        send({ id: request?.id ?? '', ok: false, error: { code: error.code ?? 'INTERNAL', message: error.message ?? String(error) } })
      }
    })
  })
  await new Promise((resolveClose) => input.once('close', resolveClose))
  await chain
  runner.dispose()
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith('runner-process.js')) {
  runStdio().catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
}
