import { AsyncLocalStorage } from 'node:async_hooks'
import { randomUUID } from 'node:crypto'
import { spawn } from 'node:child_process'
import net from 'node:net'
import { rmSync, writeFileSync, readFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { SessionId } from '@deepseek-ai/dsh-session'
import {
  NO_START_CAPABILITIES,
  subprocessRunHandle,
} from '@deepseek-ai/dsh-subagent'
import { scrubbedParentEnv } from '@deepseek-ai/dsh-subprocess'
import { temporaryEnvironment } from './environment.js'

// 统一的 mounter 二进制解析：root 直接跑 mounterBin（自带 CAP_SYS_ADMIN），
// 非 root 走 mounterCommand（通常是 sudo 包装）。`mounterBin` 默认回落到 mounterCommand。
export function resolveMounterCommand(config) {
  return process.getuid?.() === 0
    ? (config.mounterBin ?? config.mounterCommand)
    : config.mounterCommand
}

/**
 * 把子 Agent 的 prompt 序列化成纯文本（只提取 text 块），供 one-shot 子进程经 stdin 读取。
 */
function serializePrompt(prompt) {
  if (typeof prompt === 'string') return prompt
  if (!Array.isArray(prompt)) return ''
  return prompt
    .map((block) => (typeof block?.text === 'string' ? block.text : ''))
    .filter((text) => text.length > 0)
    .join('\n\n')
}

/**
 * FuseProvider — FUSE 隔离模式的子 Agent 提供方。
 *
 * 每个子 Agent 获得独立 mount namespace 和 /workspace FUSE 挂载。
 * start() 流程：mounter unshare(CLONE_NEWNS) → 递归 MS_PRIVATE →
 * 挂载固定路径 /workspace（该 Agent 的 CensorFS View）→ 降权 → exec 子 Agent。
 *
 * runtime.js 在调用前已 cli.open() 建好 Ticket + View，经 withBinding() 传入 view_id/uid/gid。
 */
export class FuseProvider {
  capabilities = NO_START_CAPABILITIES
  inheritsParentContext = false

  constructor(name, config, logger) {
    this.name = name
    this.config = config
    this.logger = logger
    this.bindings = new AsyncLocalStorage()
    // runId:variantId -> sendControl(action, payload)，供 runtime 对运行中的 variant 发控制消息
    this.controlChannels = new Map()
    // runId:variantId -> cancelChild()，供 runtime.abortVariant 真正停止 worker
    // （只 abort daemon ticket 不停 worker，卡片 aborted 而代理图一直 RUNNING）。
    this.cancelChannels = new Map()
  }

  /**
   * 取消某个运行中的 variant worker（cancel 控制消息 → 3s 未退 SIGTERM）。
   * @returns true 已触发；false 该 variant 无活动 worker（未运行或已结束）
   */
  cancel(runId, variantId) {
    const cancelChild = this.cancelChannels.get(`${runId}:${variantId}`)
    if (cancelChild === undefined) return false
    cancelChild()
    return true
  }

  /**
   * 向某个运行中的 variant 发控制消息（cancel / update-policy）。
   * @returns true 已发送；false 没有该 variant 的控制通道（未运行或已结束）
   */
  sendControl(runId, variantId, action, payload) {
    const channel = this.controlChannels.get(`${runId}:${variantId}`)
    if (channel === undefined) return false
    return channel(action, payload)
  }

/**
   * binding 经 AsyncLocalStorage 传入：runId/variantId/viewId/uid/gid/tmpDir
   */
  withBinding(binding, action) {
    return this.bindings.run(binding, action)
  }

  async start(request) {
    const binding = this.bindings.getStore()
    if (binding === undefined) {
      throw new Error('fuse provider requires a Variant binding with viewId/uid/gid')
    }
    if (request.signal.aborted) throw new Error('worker was cancelled before startup')

    const id = SessionId(randomUUID())

    // 事件流：Unix socket 服务端，收子进程经 exporter 回传的 session/event。
    // 路径经 argv 传给子进程（能穿 sudo env_reset）；exporter 据此连回来。
    // 路径只用本 provider 的 AAA id，避免 runId(36)+variantId(64) 撞 108 字节路径上限。
    const eventSocketPath = `/tmp/bfs-${id}.sock`
    try { rmSync(eventSocketPath) } catch {}
    const events = []

    // Task/result 经共享文件通道（argv 传路径）：宿主 Harness 对 plugin spawn
    // 的子进程 stdio 做协议化接管（socketpair 中继，不转发裸 stdin/stdout），
    // 因此 stdin/stdout 契约不可靠。文件放 tmpRoot（进程内可写，子进程在 mount
    // namespace 内仍可见同一宿主文件系统）。
    const taskFilePath = join(this.config.tmpRoot ?? '/tmp', `bfs-task-${id}.txt`)
    const resultFilePath = join(this.config.tmpRoot ?? '/tmp', `bfs-result-${id}.txt`)
    const taskText = serializePrompt(request.prompt)
    try {
      mkdirSync(this.config.tmpRoot ?? '/tmp', { recursive: true })
      // 0644：task 非机密，且子进程可能以非 root 降权身份读取
      writeFileSync(taskFilePath, taskText, { mode: 0o644 })
    } catch (error) {
      throw new Error(`could not write worker task file ${taskFilePath}: ${error.message}`)
    }
    try { rmSync(resultFilePath) } catch {}

    // 子进程连上来的那条连接：事件流从它读，控制消息（cancel/update-policy）往它写
    let childConn = null
    const eventServer = net.createServer((conn) => {
      childConn = conn
      let buf = ''
      conn.setEncoding('utf8')
      conn.on('data', (chunk) => {
        buf += chunk
        let nl
        while ((nl = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, nl).trim()
          buf = buf.slice(nl + 1)
          if (line.length === 0) continue
          let parsed
          try { parsed = JSON.parse(line) } catch { continue }
          events.push(parsed)
          // 实时转发：走 binding.onEvent 流式落盘，避免 runtime 闭包 childSessionId 的启动竞态。
          try { binding.onEvent?.(parsed, id) } catch {}
        }
      })
    })
    // 等 socket 真正 listening 再 spawn child，否则 exporter 立即 connect 可能失败且无重连 → 事件全丢。
    await new Promise((resolve, reject) => {
      const onError = (error) => {
        eventServer.off('listening', onListening)
        reject(error)
      }
      const onListening = () => {
        eventServer.off('error', onError)
        resolve()
      }
      eventServer.once('error', onError)
      eventServer.once('listening', onListening)
      eventServer.listen(eventSocketPath)
})

    // FS 接入点：用 censorfs-mounter 包裹子 Agent（建独立 namespace → 挂 FUSE → 降权 → exec）
    const mounterArgs = [
      '--socket', this.config.socket,
      '--view-id', binding.viewId,
      '--uid', String(binding.uid),
      '--gid', String(binding.gid),
      '--',
      this.config.childCommand,
      ...this.config.childArgs,
      // provider/model 走 argv 而非 env：mounter 经 sudo 启动，env_reset 会清空环境变量。
      // 子进程据此生成 --patch 覆盖 headless agent-default-model。API key 不经 argv，
      // 子进程重建 DSH_HOME 后读 .credentials.yaml（官方凭据层，穿得过 sudo env_reset）。
      '--model', this.config.model ?? 'deepseek-v4-flash',
      '--provider', this.config.provider ?? 'deepseek-official',
      '--event-socket', eventSocketPath,
      '--task-file', taskFilePath,
      '--result-file', resultFilePath,
    ]

    const env = {
      ...scrubbedParentEnv(),
      ...this.config.childEnv,
      ...temporaryEnvironment(binding.tmpDir),
      BRANCH_EXPLORE_RUN_ID: binding.runId,
      BRANCH_EXPLORE_VARIANT_ID: binding.variantId,
      BRANCH_EXPLORE_MODE: 'fuse',
}

    // 启动子进程：root 直接跑 mounterBin，非 root 走 sudo 包装
    const mounterBin = resolveMounterCommand(this.config)
    const child = spawn(mounterBin, mounterArgs, {
      cwd: '/workspace',
      env,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    })

    // 任务已写入 --task-file（见上），不再依赖被协议化接管的 stdin；
    // 保留 stdin 写入作为宽松环境的回退（adapter 优先读 task 文件）。
    child.stdin.on('error', () => {}) // 子进程提前退出时忽略 EPIPE
    try {
      child.stdin.write(taskText)
      child.stdin.end()
    } catch {
      // stdin 通道不可用时忽略（task 文件是权威通道）
    }

    // 控制通道：写 control 消息到子进程连上来的连接，子进程 exporter 读后落到 agent 上。
    const channelKey = `${binding.runId}:${binding.variantId}`
    const sendControl = (action, payload) => {
      if (childConn === null || childConn.destroyed) return false
      try {
        childConn.write(JSON.stringify({ type: 'control', action, ...(payload ?? {}) }) + '\n')
        return true
      } catch {
        return false
      }
    }
    this.controlChannels.set(channelKey, sendControl)

    let cancelled = false
    let cancelTimer = null
    let killTimer = null
    // exited 跟踪真实退出（child.killed 只表示信号已发出，不代表已死），
    // 供升级链判断。
    let exited = false
    child.once('close', () => { exited = true })
    // 优雅取消：先发 control 让子进程 agent 自行 cancel，3s 未退 SIGTERM 硬杀，
    // SIGTERM 也杀不动（忽略/阻塞信号）再 3s SIGKILL 兜底。
    const cancelChild = () => {
      if (cancelled) return
      cancelled = true
      sendControl('cancel')
      cancelTimer = setTimeout(() => {
        if (exited) return
        child.kill('SIGTERM')
        killTimer = setTimeout(() => { if (!exited) child.kill('SIGKILL') }, 3000)
        killTimer.unref?.()
      }, 3000)
      cancelTimer.unref?.()
    }
    const onAbort = () => { cancelChild() }
    request.signal.addEventListener('abort', onAbort, { once: true })
    this.cancelChannels.set(channelKey, cancelChild)

    // 收集子进程输出作为结果
    const result = (async () => {
      try {
        const stdoutChunks = []
        const stderrChunks = []
        child.stdout.on('data', (chunk) => stdoutChunks.push(chunk))
        child.stderr.on('data', (chunk) => stderrChunks.push(chunk))

        const exitCode = await new Promise((resolve, reject) => {
          child.once('error', reject)
          child.once('close', resolve)
        })

        const stdout = Buffer.concat(stdoutChunks).toString('utf8')
        const stderr = Buffer.concat(stderrChunks).toString('utf8')

        // 结果文件是权威通道（子进程 stdout 可能被 Harness 协议化接管）；文件为空时才回退 stdout。
        let fileResult = ''
        try { fileResult = readFileSync(resultFilePath, 'utf8') } catch {}
        try { rmSync(resultFilePath) } catch {}
        const outputText = fileResult.length > 0 ? fileResult : stdout

        if (cancelled) {
          return { output: [], stopReason: 'aborted' }
        }
        if (exitCode !== 0 && outputText.length === 0) {
          return {
            output: [{ type: 'text', text: stderr || `process exited with code ${exitCode}` }],
            stopReason: 'error',
          }
        }
        // 子进程正常退出，结果文本作为输出
        return {
          output: outputText.length > 0 ? [{ type: 'text', text: outputText }] : [],
          stopReason: 'completed',
        }
      } finally {
        request.signal.removeEventListener('abort', onAbort)
      }
    })()

    return subprocessRunHandle({
      id,
      // 子进程事件附在 result 上，供 runtime 落盘为 variant-activity
      result: result.then((r) => ({ ...r, events })),
      signal: request.signal,
      onAbort,
      requestCancel: () => { cancelChild() },
      teardown: () => {
        this.controlChannels.delete(channelKey)
        this.cancelChannels.delete(channelKey)
        if (cancelTimer !== null) clearTimeout(cancelTimer)
        if (killTimer !== null) clearTimeout(killTimer)
        if (!child.killed) child.kill('SIGTERM')
        eventServer.close()
        try { rmSync(eventSocketPath) } catch {}
        try { rmSync(taskFilePath) } catch {}
        try { rmSync(resultFilePath) } catch {}
        if (events.length > 0) {
          this.logger?.info?.(`fuse ${binding.runId}/${binding.variantId}: collected ${events.length} child events`)
        }
        return Promise.resolve()
      },
    })
  }

  prepareContinuable() {
    return Promise.resolve({})
  }
}