// 探索状态经 DSH Session 事件持久化，前端从事件重建视图
export const EVENT_TYPES = Object.freeze([
  'exploration-started',
  'variant-running',
  'variant-activity',
  'variant-sys-activity',
  'variant-prepared',
  'variant-validated',
  'variant-failed',
  'ranking-ready',
  'variant-published',
  'variant-aborted',
  'variant-stale',
  'isolation-fallback',
  'exploration-ended',
  'subagent-graph-opened',
  'subagent-started',
  'subagent-ended',
  'subagent-activity',
  'subagent-graph-cleared',
])

// 感兴趣的子 Agent 会话事件类型，用于活动追踪
export const ACTIVITY_EVENT_TYPES = new Set([
  'tool/call',
  'tool/result',
  'assistant/message',
  'step/start',
  'turn/end',
])

// 从工具参数提取目标路径
const TOOL_TARGET_KEYS = ['path', 'file_path', 'filePath', 'file', 'absolute_path', 'notebook_path']

// shell 命令中涉及文件路径的常见模式
const SHELL_PATH_PATTERNS = [
  /(?:^|[\s;&|(])(?:rm|del|Remove-Item|ri)\s+(?:-[^\s]+\s+)*([^\s;&|]+)/iu,
  /(?:^|[\s;&|(])(?:mkdir|md|New-Item)\s+(?:-[^\s]+\s+)*([^\s;&|]+)/iu,
  /(?:^|[\s;&|(])(?:cat|type|Get-Content|gc)\s+([^\s;&|]+)/iu,
  /(?:^|[\s;&|(])(?:touch|ni)\s+([^\s;&|]+)/iu,
  /(?:^|[\s;&|(])(?:python|py|node|npm|pytest|jest|vitest)\s+(?:-[^\s]+\s+)*((?:[^\s;&|]*\/)?[^\s;&|]+\.(?:py|js|mjs|cjs|ts|tsx|json|yaml|yml|toml|md|txt))\b/iu,
  /(?:^|[\s;&|(])(?:mv|move|Move-Item|mi)\s+([^\s;&|]+)\s+([^\s;&|]+)/iu,
  /(?:^|[\s;&|(])(?:cp|copy|Copy-Item|cpi)\s+([^\s;&|]+)\s+([^\s;&|]+)/iu,
]

function shellTargets(command) {
  const found = []
  for (const pattern of SHELL_PATH_PATTERNS) {
    const match = command.match(pattern)
    if (match === null) continue
    for (let i = 1; i < match.length; i += 1) {
      const value = match[i]
      if (value !== undefined && value.length > 0) found.push(value)
    }
  }
  return found.length > 0 ? [...new Set(found)].slice(0, 3) : undefined
}

function toolTarget(data) {
  if (data.target !== undefined) return data.target
  let args = data.arguments ?? data.args ?? data.input
  if (typeof args === 'string') {
    try { args = JSON.parse(args) } catch { return undefined }
  }
  if (args === null || typeof args !== 'object') return undefined
  for (const key of TOOL_TARGET_KEYS) {
    const value = args[key]
    if (typeof value === 'string' && value.length > 0) return value
  }
if (typeof args.command === 'string' && args.command.length > 0) {
    return shellTargets(args.command)?.join(' → ')
  }
  return undefined
}

// 从 subagent/descriptor 事件提取可读标签（子代理 spawn 时由宿主写入子会话）
export function labelFromDescriptor(data) {
  if (data === null || typeof data !== 'object') return undefined
  const candidates = [
    data.label,
    data.title,
    data.task,
    typeof data.prompt === 'string' ? data.prompt : undefined,
  ]
  for (const value of candidates) {
    if (typeof value === 'string' && value.trim().length > 0) return value.trim().slice(0, 120)
  }
  if (Array.isArray(data.prompt)) {
    const text = data.prompt.find((block) => block?.type === 'text')?.text
    if (typeof text === 'string' && text.trim().length > 0) return text.trim().slice(0, 120)
  }
  return undefined
}

// 子代理 stopReason → 图节点状态
export function subagentNodeStatus(stopReason) {
  switch (stopReason) {
    case 'completed': return 'completed'
    case 'aborted': return 'aborted'
    case 'refusal': return 'refusal'
    case 'max-tokens': return 'max-tokens'
    default: return 'error'
  }
}

// content 兼容 data.content 与 data.message.content 两种形状
function messageContent(data) {
  const content =
    data.content ??
    data.message?.content

  if (typeof content === 'string') {
    return content
  }

  if (Array.isArray(content)) {
    return content
      .filter((c) => c?.type === 'text')
      .map((c) => c.text)
      .join('')
  }

  return ''
}


// 长期持久化数据，对超长字符串 / 超深对象 / 巨型数组做截断
function compactValue(value, depth = 0) {
  if (
    value === null ||
    value === undefined
  ) {
    return value
  }

  if (typeof value === 'string') {
    if (value.length <= 500) {
      return value
    }

    return (
      value.slice(0, 360) +
      `… (+${value.length - 360} chars)`
    )
  }

  if (
    typeof value === 'number' ||
    typeof value === 'boolean'
  ) {
    return value
  }

  if (depth >= 3) {
    if (Array.isArray(value)) {
      return `[${value.length} items]`
    }

    return '{…}'
  }

  if (Array.isArray(value)) {
    return value
      .slice(0, 20)
      .map((item) =>
        compactValue(item, depth + 1)
      )
  }

  if (typeof value === 'object') {
    const out = {}

    const entries =
      Object.entries(value)
        .slice(0, 30)

    for (const [key, item] of entries) {
      out[key] =
        compactValue(
          item,
          depth + 1,
        )
    }

    return out
  }

  return String(value)
}


function jsonPreview(
  value,
  limit = 1200,
) {
  if (value === undefined) {
    return undefined
  }

  let text

  try {
    const compact =
      compactValue(value)

    text =
      typeof compact === 'string'
        ? compact
        : JSON.stringify(
            compact,
            null,
            2,
          )
  } catch {
    text = String(value)
  }

  if (text.length <= limit) {
    return text
  }

  return (
    text.slice(0, limit) +
    `… (+${text.length - limit} chars)`
  )
}


function toolArguments(data) {
  let args =
    data.arguments ??
    data.args ??
    data.input

  if (typeof args === 'string') {
    try {
      args = JSON.parse(args)
    } catch {
      return args
    }
  }

  return args
}


function stringField(...values) {
  for (const value of values) {
    if (
      typeof value === 'string' &&
      value.length > 0
    ) {
      return value
    }
  }

  return undefined
}


function numberField(...values) {
  for (const value of values) {
    if (
      typeof value === 'number' &&
      Number.isFinite(value)
    ) {
      return value
    }
  }

  return undefined
}


function resultText(data) {
  const message =
    messageContent(data).trim()

  if (message !== '') {
    return message
  }

  const candidates = [
    data.output,
    data.result,
    data.value,
  ]

  for (const value of candidates) {
    if (value === undefined) {
      continue
    }

    const preview =
      jsonPreview(value, 1800)

    if (
      preview !== undefined &&
      preview.trim() !== ''
    ) {
      return preview
    }
  }

  return ''
}


// 原始会话事件 → 可长期存储的 Activity V2
export function summarizeActivityEvent(
  event,
) {
  const type = event.type
  const data = event.data ?? {}

  if (type === 'tool/call') {
    const args =
      toolArguments(data)

    const target =
      toolTarget(data)

    const command =
      stringField(
        data.command,
        args?.command,
        args?.cmd,
      )

    const cwd =
      stringField(
        data.cwd,
        args?.cwd,
        args?.workingDirectory,
        args?.working_directory,
      )

    const argsPreview =
      jsonPreview(args, 1400)

    const callId =
      stringField(
        data.toolCallId,
        data.callId,
      )

    return {
      type,

      tool:
        data.tool ??
        data.name,

      ...(target === undefined
        ? {}
        : { target }),

      ...(command === undefined
        ? {}
        : {
            command:
              command.slice(0, 1200),
          }),

      ...(cwd === undefined
        ? {}
        : { cwd }),

      ...(argsPreview === undefined
        ? {}
        : { argsPreview }),

      ...(callId === undefined
        ? {}
        : { callId }),
    }
  }


  if (type === 'tool/result') {
    const text =
      resultText(data).trim()

    const callId =
      stringField(
        data.toolCallId,
        data.callId,
      )

    const exitCode =
      numberField(
        data.exitCode,
        data.exit_code,
      )

    const durationMs =
      numberField(
        data.durationMs,
        data.elapsedMs,
      )

    return {
      type,

      tool:
        data.tool ??
        data.name,

      ok:
        data.ok !== false,

      ...(callId === undefined
        ? {}
        : { callId }),

      ...(exitCode === undefined
        ? {}
        : { exitCode }),

      ...(durationMs === undefined
        ? {}
        : { durationMs }),

      ...(text === ''
        ? {}
        : {
            preview:
              text.slice(0, 1800),
          }),
    }
  }


  if (
    type ===
    'assistant/message'
  ) {
    const text =
      messageContent(data)

    return {
      type,
      preview:
        text.slice(0, 1400),
    }
  }


  if (type === 'step/start') {
    return {
      type,
      step: data.step,

      ...(data.model === undefined
        ? {}
        : {
            model:
              String(data.model)
                .slice(0, 120),
          }),
    }
  }


  if (type === 'turn/end') {
    return {
      type,
      reason: data.reason,

      ...(data.usage === undefined
        ? {}
        : {
            usagePreview:
              jsonPreview(
                data.usage,
                600,
              ),
          }),
    }
  }

  return { type }
}

export function isBranchExploreEvent(event) {
  return EVENT_TYPES.includes(event?.type) && typeof event?.data?.runId === 'string'
}

export const isCensorFsEvent = isBranchExploreEvent

// 折叠一次探索的完整状态
export function foldExploration(events, wantedRunId) {
  let state
  for (const event of events) {
    if (!isBranchExploreEvent(event)) continue
    const data = event.data
    if (wantedRunId !== undefined && data.runId !== wantedRunId) continue

    if (event.type === 'exploration-started') {
      state = {
        runId: data.runId,
        task: data.task,
        branch: data.branch,
        fsEnabled: data.fsEnabled ?? true,
        mode: data.mode ?? 'fuse',
        expectedHead: data.expectedHead,
        validationProfile: data.validationProfile,
        previewProfile: data.previewProfile,
        strategies: data.strategies ?? [],
        isolation: data.isolation,
        isolationFallbacks: [],
        startedAt: data.startedAt,
        variants: {},
        ranking: [],
        status: 'running',
      }
      continue
    }

    if (state === undefined || data.runId !== state.runId) continue

    if (event.type === 'variant-running') {
      state.variants[data.variantId] = {
        ...data,
        status: 'running',
        activities: [],
        sysActivities: [],
      }
    } else if (event.type === 'variant-activity') {
      const variant = state.variants[data.variantId]
      if (variant !== undefined) {
        variant.activities = [...(variant.activities ?? []), data.activity]
        variant.lastActivity = data.activity
      }
    } else if (event.type === 'variant-sys-activity') {
      const variant = state.variants[data.variantId]
      if (variant !== undefined) {
        variant.sysActivities = [...(variant.sysActivities ?? []), data.activity]
        variant.lastSysActivity = data.activity
      }
    } else if (event.type === 'variant-prepared') {
      state.variants[data.variantId] = {
        ...state.variants[data.variantId],
        ...data,
        status: data.validation?.requiredPassed === false ? 'validation-failed' : 'prepared',
      }
    } else if (event.type === 'variant-validated') {
      const validation = data.validation
      const status = validation?.status === 'passed'
        ? 'prepared'
        : validation?.status === 'failed'
          ? 'validation-failed'
          : 'unvalidated'
      state.variants[data.variantId] = {
        ...state.variants[data.variantId],
        ...data,
        validation,
        status,
      }
    } else if (event.type === 'variant-failed') {
      state.variants[data.variantId] = {
        ...state.variants[data.variantId],
        ...data,
        status: 'failed',
      }
    } else if (event.type === 'isolation-fallback') {
      const variant = state.variants[data.variantId]
      if (variant !== undefined) variant.isolationFallback = data
      state.isolationFallbacks = [...(state.isolationFallbacks ?? []), data]
      if (state.isolation !== undefined) {
        const { cgroupRoot, ...isolation } = state.isolation
        state.isolation = {
          ...isolation,
          effectiveLevel: 'process',
          cgroupEnabled: false,
          warnings: [...(isolation.warnings ?? []), data.warning],
        }
      }
    } else if (event.type === 'ranking-ready') {
      state.ranking = data.ranking
      state.status = 'ranked'
    } else if (event.type === 'variant-published') {
      state.variants[data.variantId] = {
        ...state.variants[data.variantId],
        ...data,
        status: 'published',
      }
      state.status = 'published'
    } else if (event.type === 'variant-aborted') {
      state.variants[data.variantId] = {
        ...state.variants[data.variantId],
        ...data,
        status: 'aborted',
      }
    } else if (event.type === 'variant-stale') {
      state.variants[data.variantId] = {
        ...state.variants[data.variantId],
        ...data,
        status: 'stale',
      }
      state.status = 'stale'
    } else if (event.type === 'exploration-ended') {
      state.ended = data
      if (state.status === 'running') state.status = data.status
    }
  }
  return state
}

// 本进程启动时间：早于它的 running 状态都是上一个进程的残留
// （worker 随旧进程死亡，但 terminal 事件没来得及落盘）。
export const PROCESS_STARTED_AT = Date.now()

// 重启残留 reconcile（图级）：扫描根会话，凡 PROCESS_STARTED_AT 之前启动、
// 且至今没有 subagent-ended 的节点，补写 ended（stopReason 'error'）。
// 幂等：补写后 ended 集合含该 sessionId，重复调用无操作。
// @returns 补写数量
export function reconcileInterruptedSubagents(session, processStartedAt = PROCESS_STARTED_AT) {
  // rc.2 的 Session 用 snapshotEvents() 取事件快照（不再有 events 数组属性）。
  const log = session.snapshotEvents()
  const ended = new Set()
  for (const event of log) {
    if (event.type === 'subagent-ended') ended.add(event.data?.sessionId)
  }
  const stale = []
  for (const event of log) {
    if (event.type !== 'subagent-started') continue
    const sessionId = event.data?.sessionId
    const startedAt = event.data?.startedAt ?? event.time ?? 0
    if (sessionId === undefined || ended.has(sessionId) || startedAt >= processStartedAt) continue
    ended.add(sessionId)
    stale.push(sessionId)
  }
  for (const sessionId of stale) {
    session.append('subagent-ended', {
      sessionId,
      rootSessionId: session.id,
      stopReason: 'error',
      endedAt: Date.now(),
    })
  }
  return stale.length
}

// 确定性排名：未验证的 variant 也能参与排名，只有明确验证失败的被排除。
// 分层排序：通过（passed）优先于未验证（unvalidated），层内再比失败检查数、变更路径数、耗时。
function validationTier(variant) {
  return variant.validation?.status === 'passed' ? 0 : 1
}

export function rankVariants(variants) {
  return [...variants]
    .filter((variant) =>
      variant.candidate !== undefined &&
      variant.validation?.status !== 'failed' &&
      variant.validation?.requiredPassed !== false)
    .sort((left, right) => {
      const leftTier = validationTier(left)
      const rightTier = validationTier(right)
      if (leftTier !== rightTier) return leftTier - rightTier
      const leftFailures = left.validation?.checks?.filter((check) => !check.passed).length ?? 0
      const rightFailures = right.validation?.checks?.filter((check) => !check.passed).length ?? 0
      if (leftFailures !== rightFailures) return leftFailures - rightFailures
      const leftChanges = left.pathDiff?.length ?? Number.MAX_SAFE_INTEGER
      const rightChanges = right.pathDiff?.length ?? Number.MAX_SAFE_INTEGER
      if (leftChanges !== rightChanges) return leftChanges - rightChanges
      return (left.durationMs ?? Number.MAX_SAFE_INTEGER) - (right.durationMs ?? Number.MAX_SAFE_INTEGER)
    })
    .map((variant, index) => ({
      rank: index + 1,
      variantId: variant.variantId,
      requiredPassed: variant.validation?.requiredPassed ?? null,
      changedPaths: variant.pathDiff?.length ?? 0,
      durationMs: variant.durationMs,
    }))
}

// child 事件 → 应落盘到根会话的图事件。
// 身份判据：带 parentSessionId 保留真身；否则（child 根）投影为 childSessionId。
// 返回 null（丢弃）或 [{ type, data }]。
export function projectFuseEvent(rootSessionId, childSessionId, nested, raw, now = Date.now()) {
  const isNested = (id) => typeof id === 'string' && nested.has(id)

  if (raw.type === 'subagent-start') {
    const childId = raw.sessionId
    if (typeof childId === 'string' && childId.length > 0) nested.add(childId)
    const parent = isNested(raw.parentSessionId) ? raw.parentSessionId : childSessionId
    return [{
      type: 'subagent-started',
      data: {
        sessionId: childId,
        parentSessionId: parent,
        rootSessionId,
        provider: raw.provider,
        local: false,
        runId: raw.runId,
        variantId: raw.variantId,
        ...(raw.label === undefined ? {} : { label: raw.label }),
        startedAt: raw.startedAt ?? now,
      },
    }]
  }

  if (raw.type === 'subagent-end') {
    return [{
      type: 'subagent-ended',
      data: {
        sessionId: raw.sessionId,
        rootSessionId,
        stopReason: raw.stopReason,
        endedAt: raw.endedAt ?? now,
      },
    }]
  }

  if (raw.type !== 'event') return null
  if (!ACTIVITY_EVENT_TYPES.has(raw.event)) return null

  const summary = summarizeActivityEvent({ type: raw.event, data: raw.data ?? null })
  const activity = { ...summary, at: now }
  const sessionId = (typeof raw.parentSessionId === 'string' && raw.parentSessionId.length > 0)
    ? raw.sessionId
    : childSessionId
  return [
    { type: 'subagent-activity', data: { sessionId, rootSessionId, activity } },
    { type: 'variant-activity', data: { runId: raw.runId, variantId: raw.variantId, activity } },
  ]
}