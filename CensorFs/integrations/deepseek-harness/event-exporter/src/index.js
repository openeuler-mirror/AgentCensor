import net from 'node:net'
import { randomUUID } from 'node:crypto'

// 子进程侧"记者"插件：监听本进程的全局 session/event，转成 JSON 逐行写到 Unix socket，
// 同时反向读父进程（FuseProvider）经同一 socket 发来的控制消息（cancel / update-policy），
// 把控制动作落到本进程正在运行的 agent 上（优雅取消 / 运行中改策略）。
export const name = 'event-exporter'
export const inject = []

function safeJson(value) {
  try {
    return JSON.stringify(value)
  } catch {
    return undefined
  }
}

// 逐行解析（两端对称）：控制消息也是 JSON line，跟事件流共用同一条 socket
function parseLines(onLine) {
  let buf = ''
  return (chunk) => {
    buf += chunk
    let nl
    while ((nl = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, nl).trim()
      buf = buf.slice(nl + 1)
      if (line.length === 0) continue
      try { onLine(JSON.parse(line)) } catch {}
    }
  }
}

// 构造一条带新 id 的 user message，供 agent.steer 注入运行中策略指令。
// 不 import @deepseek-ai/dsh-llm（避免新增 peerDep 解析风险），直接拼消息对象。
function makeUserMessage(text) {
  return {
    id: randomUUID(),
    role: 'user',
    content: [{ type: 'text', text }],
    source: { kind: 'plugin', plugin: name },
  }
}

export function apply(ctx, config) {
  const socketPath = config?.socket ?? process.env.DSH_EVENT_SOCKET
  if (!config?.enabled || !socketPath) return

  const sock = net.createConnection(socketPath)
  sock.on('error', () => {})
  // 关键：unref 让 socket 不阻止进程退出。exporter 只是"记者"，不是主工作；
  // 否则这个常驻连接会把 headless 的事件循环吊住，任务完成后进程迟迟不退出。
  // unref 只影响 keep-alive，不影响可读性 —— 控制消息在 agent 运行期间仍能读到。
  sock.unref()

  const send = (obj) => {
    const line = safeJson(obj)
    if (line !== undefined) sock.write(line + '\n')
  }

  // 反向控制：读父进程经 socket 发来的控制消息。agent 在 headless-runner 里
  // 异步创建，控制消息只会在子进程开始流事件（即 agent 已跑起来）之后到达，
  // 所以这里用 ctx.get('agents').list() 能拿到当前唯一那个 agent。
  sock.on('data', parseLines((msg) => {
    if (msg?.type !== 'control') return
    const agents = ctx.get('agents')?.list?.() ?? []
    if (msg.action === 'cancel') {
      // 优雅取消：让当前 agent 结束本轮并退出，而不是被父进程 SIGTERM 硬杀
      for (const agent of agents) {
        try { agent.cancel({ kind: 'parent' }) } catch {}
      }
    } else if (msg.action === 'update-policy') {
      // 运行中更新策略：把策略指令作为 steering 注入当前 agent，令其立即改向
      const directive = msg.policy?.directive
      if (typeof directive !== 'string' || directive.trim() === '') return
      for (const agent of agents) {
        try { agent.steer(makeUserMessage(directive)) } catch {}
      }
    }
  }))

  // 全局监听，接收本进程所有 session 的事件（工具调用 / 助手消息 / turn 结束等）。
  // parentSessionId 让父侧身份投影抗乱序：CC 的 activity 可能早于 subagent/start
  // 到达，父侧靠"消息自带 parentSessionId"判它是不是 child 内部子 session。
  ctx.on('session/event', (session, event) => {
    const parentSessionId = session.header?.parentSession
    send({
      type: 'event',
      sessionId: session.id,
      ...(parentSessionId === undefined ? {} : { parentSessionId }),
      event: event.type,
      data: event.data ?? null,
    })
  }, { global: true })

  const runId = process.env.BRANCH_EXPLORE_RUN_ID
  const variantId = process.env.BRANCH_EXPLORE_VARIANT_ID
  const meta = (runId === undefined && variantId === undefined) ? {} : {
    ...(runId === undefined ? {} : { runId }),
    ...(variantId === undefined ? {} : { variantId }),
  }

  ctx.on('subagent/start', (info) => {
    const parentSessionId = ctx.get('sessions')?.get(info.id)?.header?.parentSession
    send({
      type: 'subagent-start',
      sessionId: info.id,
      ...(parentSessionId === undefined ? {} : { parentSessionId }),
      ...(info.label === undefined ? {} : { label: info.label }),
      ...(info.provider === undefined ? {} : { provider: info.provider }),
      ...meta,
    })
  }, { global: true })

  ctx.on('subagent/end', (info) => {
    send({
      type: 'subagent-end',
      sessionId: info.id,
      ...(info.stopReason === undefined ? {} : { stopReason: info.stopReason }),
    })
  }, { global: true })
}

const plugin = { apply, name, inject }
export default plugin
