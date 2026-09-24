import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { defineTool } from '@deepseek-ai/dsh-tools'
import { KNOWN_SESSION_EVENT_TYPES } from '@deepseek-ai/dsh-session'
import { resolveChildCommand } from './child-command.js'
import { CensorFsCli } from './censorfs-cli.js'
import { CombinedBranchExploreRuntime } from './combined-runtime.js'
import { FuseProvider } from './fuse-provider.js'
import { InProcessCensorFsProvider } from './in-process-provider.js'
import { ParallelWorldsRuntime } from './namespace-runtime.js'
import { normalizeIsolationConfig, probeRunnerEnvironment } from './runner-environment.js'
import { RunnerManager } from './runner-manager.js'
import { registerRunnerToolProxy } from './runner-tool-proxy.js'
import { resolveEnabledTracks } from './track-config.js'
import { BranchExploreRuntime } from './runtime.js'

export const name = 'censorfs-parallel-worlds'
export const inject = ['tools', 'commands', 'subagents', 'sessions']

// 插件写入会话日志的自定义事件类型（见 src/events.js）。
// dsh 0.1.5-rc.2：持久化读取器的放行条件是
// `KNOWN_SESSION_EVENT_TYPES.has(type) || event.ignorable === true`
// （session-persistence/src/storage-contract.ts），而官方的 ignorable 信封
// 只能由 Session.append 内部产生、没有公开的插件写入口，事件名注册机制也
// 已被官方否决（known-event-types.ts 头注）。运行时 Set 未冻结，本插件经
// profile node_modules 链接到 harness 安装内的同一份 dsh-session 模块
// （Node 按真实路径解析，模块实例共享），因此在 apply() 时向
// KNOWN_SESSION_EVENT_TYPES 注册即可同时通过写入与读取校验。
// 迁移方向：待官方提供 ignorable 事件写入口后改走信封标记。
const PLUGIN_EVENT_TYPES = [
  'exploration-started',
  'variant-running',
  'variant-activity',
  'variant-sys-activity',
  'variant-prepared',
  'variant-validated',
  'variant-failed',
  'variant-published',
  'variant-aborted',
  'variant-stale',
  'isolation-fallback',
  'ranking-ready',
  'exploration-ended',
  'subagent-graph-opened',
  'subagent-started',
  'subagent-ended',
  'subagent-activity',
  'subagent-graph-cleared',
]

function registerEventTypes() {
  for (const type of PLUGIN_EVENT_TYPES) KNOWN_SESSION_EVENT_TYPES.add(type)
}

export { resolveEnabledTracks } from './track-config.js'

export function normalizeConfig(config) {
  const inProcessOnly = config.inProcessOnly === true
  const fuseOnly = config.fuseOnly === true
  resolveEnabledTracks(config)
  const required = ['socket', 'mounterCommand', 'controlPlaneCwd']
  if (!inProcessOnly) required.push('childCommand', 'provider', 'model')
  for (const key of required) {
    if (typeof config?.[key] !== 'string' || config[key].length === 0) {
      throw new TypeError(`dsh-branch-explore config.${key} is required`)
    }
  }
  const childArgs = config.childArgs ?? []
  if (!Array.isArray(childArgs) || !childArgs.every((value) => typeof value === 'string')) {
    throw new TypeError('dsh-branch-explore childArgs must be an array of strings')
  }
  const childEnv = config.childEnv ?? {}
  if (typeof childEnv !== 'object' || childEnv === null
    || !Object.values(childEnv).every((value) => typeof value === 'string')) {
    throw new TypeError('dsh-branch-explore childEnv values must be strings')
  }
  if (config.maxTokens !== undefined && (!Number.isSafeInteger(config.maxTokens) || config.maxTokens <= 0)) {
    throw new TypeError('dsh-branch-explore maxTokens must be a positive safe integer')
  }
  if (config.runnerReadyTimeoutMs !== undefined
    && (!Number.isSafeInteger(config.runnerReadyTimeoutMs) || config.runnerReadyTimeoutMs <= 0)) {
    throw new TypeError('dsh-branch-explore runnerReadyTimeoutMs must be a positive safe integer')
  }
  if (config.inProcessMaxDepth !== undefined
    && (!Number.isSafeInteger(config.inProcessMaxDepth) || config.inProcessMaxDepth < 0 || config.inProcessMaxDepth > 1)) {
    throw new TypeError('dsh-branch-explore inProcessMaxDepth must be 0 or 1; nested Runner depth > 1 is not supported')
  }
  if (typeof config.validationProfiles !== 'object' || config.validationProfiles === null) {
    throw new TypeError('dsh-branch-explore validationProfiles is required')
  }
  const defaultValidationProfile = config.defaultValidationProfile ?? undefined
  if (defaultValidationProfile !== undefined
    && (typeof defaultValidationProfile !== 'string' || defaultValidationProfile.length === 0)) {
    throw new TypeError('dsh-branch-explore defaultValidationProfile must be a non-empty string')
  }
  const defaultPreviewProfile = config.defaultPreviewProfile ?? undefined
  if (defaultPreviewProfile !== undefined
    && (typeof defaultPreviewProfile !== 'string' || defaultPreviewProfile.length === 0)) {
    throw new TypeError('dsh-branch-explore defaultPreviewProfile must be a non-empty string')
  }
  const mounterBin = config.mounterBin ?? undefined
  if (mounterBin !== undefined && (typeof mounterBin !== 'string' || mounterBin.length === 0)) {
    throw new TypeError('dsh-branch-explore mounterBin must be a non-empty string')
  }
  const runnerCgroup = config.runnerCgroup ?? {}
  if (typeof runnerCgroup !== 'object' || runnerCgroup === null || Array.isArray(runnerCgroup)) {
    throw new TypeError('dsh-branch-explore runnerCgroup must be an object')
  }
  const runnerIsolation = normalizeIsolationConfig(config.runnerIsolation, runnerCgroup)
  return {
    fsEnabled: true,
    branch: config.branch ?? 'main',
    censorfsCommand: config.censorfsCommand ?? 'censorfs',
    socket: config.socket,
    mounterCommand: config.mounterCommand,
    mounterBin,
    childCommand: resolveChildCommand(config.childCommand),
    childArgs,
    controlPlaneCwd: config.controlPlaneCwd,
    provider: config.provider,
    model: config.model,
    maxTokens: config.maxTokens,
    childEnv,
    validationProfiles: config.validationProfiles,
    defaultValidationProfile,
    previewProfiles: config.previewProfiles ?? {},
    defaultPreviewProfile,
    tmpRoot: config.tmpRoot ?? '/tmp/dsh-branch-explore',
    providerName: config.providerName ?? 'branch-explore',
    inProcessProviderName: config.inProcessProviderName ?? 'censorfs-inprocess',
    inProcessOnly,
    fuseOnly,
    runnerReadyTimeoutMs: config.runnerReadyTimeoutMs ?? 10000,
    inProcessMaxDepth: config.inProcessMaxDepth ?? 1,
    runnerIsolation,
    runnerAuditDir: config.runnerAuditDir ?? process.env.CENSORFS_RUNNER_AUDIT_DIR,
    runnerCgroup: {
      enabled: runnerCgroup.enabled === true,
      root: runnerCgroup.root,
      stateDir: runnerCgroup.stateDir,
      memoryMax: runnerCgroup.memoryMax,
      pidsMax: runnerCgroup.pidsMax,
      cpuMax: runnerCgroup.cpuMax,
      cleanupTimeoutMs: runnerCgroup.cleanupTimeoutMs ?? 5000,
    },
  }
}

function parseExploreInput(raw, defaultMode = 'fuse') {
  const input = raw.trim()
  const match = /^(?:(\d+)\s+)?(?:--mode[=\s]+(\S+)\s+)?([\s\S]+)$/u.exec(input)
  if (match === null) throw new Error('Usage: /explore [2-4] [--mode fuse|inprocess] <task>')
  const count = match[1] === undefined ? 3 : Number(match[1])
  if (!Number.isSafeInteger(count) || count < 2 || count > 4) {
    throw new Error('explore count must be between 2 and 4')
  }
  const mode = match[2] === undefined ? defaultMode : match[2]
  if (mode !== 'fuse' && mode !== 'inprocess') {
    throw new Error('explore mode must be "fuse" or "inprocess"')
  }
  return { count, task: match[3].trim(), mode }
}

function splitArgs(raw) {
  return raw.trim().split(/\s+/u).filter(Boolean)
}

function formatDoctor(report) {
  const lines = ['CensorFS Runner doctor (read-only probe; nothing was started or changed)']
  lines.push(`  mode: ${report.requestedMode} · minimumLevel: ${report.minimumLevel} · effective: ${report.error === undefined ? report.effectiveLevel : 'n/a'}`)
  if (report.error !== undefined) lines.push(`  FAILED CLOSED: ${report.error}`)
  else if (report.cgroupEnabled === true) {
    lines.push(`  cgroup v2: delegated scope under ${report.cgroupRoot} (controllers: ${(report.controllers ?? []).join(', ') || 'none'})`)
  }
  const daemon = report.daemon
  lines.push(`  daemon socket: ${daemon?.available === true
    ? `${daemon.socket}${daemon.live === true ? ' (live)' : ' (exists, no listener)'}`
    : daemon?.reason ?? 'not configured'}`)
  lines.push(`  censorfs: ${report.commands?.censorfs ?? 'NOT FOUND'}`)
  lines.push(`  mounter: ${report.commands?.mounter ?? 'NOT FOUND'}`)
  lines.push(`  node: ${report.commands?.node ?? 'NOT FOUND'}`)
  lines.push(`  bwrap: ${report.commands?.bwrap ?? 'NOT FOUND (runner bash will fail closed)'}`)
  lines.push(`  /dev/fuse: ${report.fuse === true ? 'available' : 'unavailable'}`)
  if ((report.warnings ?? []).length > 0) {
    lines.push('  warnings:')
    for (const warning of report.warnings) lines.push(`    - ${warning}`)
  }
  return lines.join('\n')
}

function registerCommands(ctx, runtime, config) {
  const tracks = resolveEnabledTracks(config)
  // FUSE 轨是默认档：双轨部署默认 fuse；仅当 FUSE 轨被禁用（inProcessOnly）才回落 inprocess。
  const defaultExploreMode = tracks.fuse ? 'fuse' : 'inprocess'
  ctx.commands.register({
    name: 'censorfs-doctor',
    description: 'probe CensorFS runtime prerequisites; use --json for machine-readable output (exitCode 0=healthy, 1=not ready)',
    async handler(invocation) {
      const report = await runtime.doctor()
      const json = splitArgs(invocation.rawInput).includes('--json')
      const healthy = report.error === undefined
        && report.daemon?.live === true
        && report.commands?.censorfs !== undefined
        && report.commands?.mounter !== undefined
        && report.commands?.bwrap !== undefined
        && report.fuse === true
      return {
        kind: healthy ? 'success' : 'error',
        // dsh 0.1.5-rc.2 的 CommandResult 只有 {kind, text}；机器可读的
        // exitCode 保留在 --json 的文本负载里。
        text: json ? JSON.stringify({ ...report, healthy, exitCode: healthy ? 0 : 1 }) : formatDoctor(report),
      }
    },
  })

  // Phase 0 调试：dump 根会话的全部 subagent-* 原始事件，
  // 确认 agent-teams label 真实格式 / parentSessionId 链 / activity 摘要内容。
  ctx.commands.register({
    name: 'subagent-dump',
    description: 'dump raw subagent-* events of the root session (debug)',
    handler(invocation) {
      const sessionId = invocation.agent.session.id
      const root = runtime.resolveRootSession(sessionId) ?? ctx.sessions.get(sessionId)
      if (root === undefined) return { kind: 'error', text: 'no root session found' }
      // rc.2 的 Session 用 snapshotEvents() 取事件快照（不再有 events 数组属性）。
      const events = root.snapshotEvents().filter((e) => typeof e.type === 'string' && e.type.startsWith('subagent-'))
      if (events.length === 0) return { kind: 'success', text: 'no subagent-* events in root session ' + root.id }
      const lines = ['root session: ' + root.id, 'events: ' + events.length, '---']
      for (const event of events.slice(-120)) {
        const d = event.data ?? {}
        lines.push(event.type + ' @' + new Date(d.startedAt ?? d.endedAt ?? (d.activity && d.activity.at) ?? event.at ?? Date.now()).toISOString().slice(11, 19))
        lines.push('  session=' + String(d.sessionId ?? '').slice(0, 12) + ' parent=' + String(d.parentSessionId ?? '-').slice(0, 12) + ' runId=' + String(d.runId ?? '-').slice(0, 12) + (d.stopReason !== undefined ? ' stop=' + d.stopReason : ''))
        if (d.activity !== undefined) {
          const a = d.activity
          lines.push('  act: type=' + a.type + ' tool=' + String(a.tool ?? '-') + ' target=' + String(a.target ?? '-') + ((a.sys === true || d.sys === true) ? ' [sys]' : ''))
          if (a.preview !== undefined) lines.push('  preview: ' + String(a.preview).slice(0, 120))
          if (a.runTask !== undefined) lines.push('  runTask: ' + String(a.runTask).slice(0, 80) + ' variant=' + String(a.variantId ?? '-'))
        }
      }
      return { kind: 'success', text: lines.join('\n') }
    },
  })

  ctx.commands.register({
    name: 'subagent-graph-clear',
    description: 'clear the global subagent graph of the root session (removes leftover nodes of deleted subagents)',
    handler(invocation) {
      const ok = runtime.clearSubagentGraph(invocation.agent.session.id)
      return ok
        ? { kind: 'success', text: 'subagent graph cleared. reload the session view to see it reset.' }
        : { kind: 'error', text: 'no root session found' }
    },
  })

  // 渲染前先用真实数据验证显示层级：parent→child 来自 parentSessionId 链，
  // 分组信号来自 runId（explore）/ label 约定（agent-teams:{teamId}:{member}）
  ctx.commands.register({
    name: 'subagent-graph-layout',
    description: 'print the display hierarchy the web graph will render (verify parent→child before rendering)',
    handler(invocation) {
      const sessionId = invocation.agent.session.id
      const root = runtime.resolveRootSession(sessionId) ?? ctx.sessions.get(sessionId)
      if (root === undefined) return { kind: 'error', text: 'no root session found' }
      // rc.2 的 Session 用 snapshotEvents() 取事件快照（不再有 events 数组属性）。
      const events = root.snapshotEvents().filter((e) => typeof e.type === 'string' && e.type.startsWith('subagent-'))
      const nodes = new Map()
      for (const event of events) {
        const d = event.data ?? {}
        if (event.type === 'subagent-started') {
          nodes.set(d.sessionId, { sessionId: d.sessionId, parentSessionId: d.parentSessionId, runId: d.runId, label: d.label, startedAt: d.startedAt, activities: [] })
        } else if (event.type === 'subagent-activity' && nodes.has(d.sessionId)) {
          nodes.get(d.sessionId).activities.push(d.activity)
        }
      }
      if (nodes.size === 0) return { kind: 'success', text: 'no tracked subagents in root session ' + root.id }
      const teamLabelOf = (node) => /^agent-teams:([^:]+):/.exec(String(node.label ?? ''))?.[1]
      const lines = ['display hierarchy (Y=execution order, X=branch depth):', 'main']
      const nodeIds = new Set(nodes.keys())
      const walk = (parentKey, depth) => {
        const children = [...nodes.values()]
          .filter((n) => (n.parentSessionId !== undefined && nodeIds.has(n.parentSessionId) ? n.parentSessionId : 'main') === parentKey)
          .sort((a, b) => (a.startedAt ?? 0) - (b.startedAt ?? 0))
        for (const child of children) {
          const team = teamLabelOf(child)
          const tag = child.runId !== undefined ? 'EXPLORE/' + String(child.runId).slice(0, 8)
            : team !== undefined ? 'TEAM:' + team
            : (child.label ?? 'SUBAGENT')
          lines.push('  '.repeat(depth + 1) + '└─ ' + tag + ' · ' + String(child.sessionId).slice(0, 12) + ' · ' + child.activities.length + ' acts')
          walk(child.sessionId, depth + 1)
        }
      }
      walk('main', 0)
      return { kind: 'success', text: lines.join('\n') }
    },
  })

  ctx.commands.register({
    name: 'censorfs-preview',
    description: 'start an expiring read-only Candidate preview without invoking the model',
    input: { hint: '<run-id> <variant-id>' },
    async handler(invocation) {
      const args = splitArgs(invocation.rawInput)
      if (args.length !== 2) return { kind: 'error', text: 'Usage: /censorfs-preview <run-id> <variant-id>' }
      try {
        const result = await runtime.preview(invocation.agent, args[0], args[1])
        return { kind: 'success', text: result.url === undefined
          ? `Candidate ${result.candidateId}, Generation ${result.generationId} is ready for read-only inspection.`
          : `Preview: ${result.url}` }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })

  ctx.commands.register({
    name: 'censorfs-validate',
    description: 're-run validation on a prepared Candidate with an explicit profile (or the default). No profile → the Candidate stays unvalidated.',
    input: { hint: '<run-id> <variant-id> [profile]' },
    async handler(invocation) {
      const args = splitArgs(invocation.rawInput)
      if (args.length < 2 || args.length > 3) return { kind: 'error', text: 'Usage: /censorfs-validate <run-id> <variant-id> [profile]' }
      try {
        const validation = await runtime.validate(invocation.agent, args[0], args[1], args[2])
        if (validation.status === 'unvalidated') {
          return { kind: 'success', text: `Variant ${args[1]} is unvalidated (no validation profile selected). Its Candidate remains frozen and readable.` }
        }
        const failed = (validation.checks ?? []).filter((check) => !check.passed).map((check) => check.name)
        return {
          kind: 'success',
          text: `Variant ${args[1]} validation (${validation.profile}): ${validation.status === 'passed' ? 'PASSED' : 'FAILED'}.\n` +
            (failed.length > 0 ? `Failed checks: ${failed.join(', ')}` : ''),
        }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })

  ctx.commands.register({
    name: 'explore',
    description: 'run 2–4 isolated branch exploration worlds',
    input: { hint: '[2-4] [--mode fuse|inprocess] <task>' },
    handler(invocation) {
      try {
        const { count, task, mode } = parseExploreInput(invocation.rawInput, defaultExploreMode)
        if (mode === 'fuse' && !tracks.fuse) {
          return { kind: 'error', text: 'The FUSE track is disabled in this deployment (fuseOnly=true); use --mode inprocess.' }
        }
        if (mode === 'inprocess' && !tracks.runner) {
          return { kind: 'error', text: 'The in-process track is disabled in this deployment (inProcessOnly=true); use --mode fuse.' }
        }
        const tool = mode === 'inprocess' ? 'branch_explore_inprocess' : 'branch_explore'
        invocation.agent.followup(createUserMessage({
          content: [{
            type: 'text',
            text: [
              `The user requested a Branch Exploration with ${count} variants (mode: ${mode}).`,
              `Task: ${task}`,
              `Propose exactly ${count} materially different implementation strategies, then call ${tool} once.`,
              'Do not guess a validation profile.',
              'Only pass validationProfile when the user explicitly requested one. Otherwise leave it unset; the runtime may use defaultValidationProfile or skip validation.',
              'After the tool returns, do not reproduce detailed World summaries, diffs, validation logs, or ranking in chat. Reply briefly that the exploration is complete and can be reviewed in Worlds. Do not publish automatically.',
            ].join('\n\n'),
          }],
          source: { kind: 'plugin', plugin: name },
        }))
        return { kind: 'success', text: `Starting ${count} branch exploration worlds (${mode}) for: ${task}` }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })

  ctx.commands.register({
    name: 'censorfs-publish',
    description: 'publish one prepared Candidate without invoking the model',
    input: { hint: '<run-id> <variant-id> [--force]' },
    async handler(invocation) {
      const args = splitArgs(invocation.rawInput)
      const force = args.includes('--force')
      const values = args.filter((value) => value !== '--force')
      if (values.length !== 2) return { kind: 'error', text: 'Usage: /censorfs-publish <run-id> <variant-id> [--force]' }
      try {
        const result = await runtime.publish(invocation.agent, values[0], values[1], force)
        return {
          kind: 'success',
          text: `Published ${values[1]} as Generation ${result.receipt.new_generation}; Head sequence is ${result.receipt.head_seq}.`,
        }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })

  ctx.commands.register({
    name: 'censorfs-abort',
    description: 'abort one or all unselected variants without invoking the model',
    input: { hint: '<run-id> [variant-id]' },
    async handler(invocation) {
      const args = splitArgs(invocation.rawInput)
      if (args.length < 1 || args.length > 2) return { kind: 'error', text: 'Usage: /censorfs-abort <run-id> [variant-id]' }
      try {
        await runtime.abort(invocation.agent, args[0], args[1])
        return { kind: 'success', text: args[1] === undefined ? 'All remaining variants were aborted.' : `Variant ${args[1]} was aborted.` }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })

  // 只读：返回 run 的权威状态。客户端打开 Worlds 视图/卡片时调用 ——
  // state() 读路径上的 reconcile 会顺手把重启残留的假 running 补写成
  // failed/ended，新事件经订阅推回客户端，旧会话的显示随之自愈。
  ctx.commands.register({
    name: 'censorfs-state',
    description: 'read the authoritative state of an exploration run without invoking the model',
    input: { hint: '<run-id>' },
    async handler(invocation) {
      const args = splitArgs(invocation.rawInput)
      if (args.length !== 1) return { kind: 'error', text: 'Usage: /censorfs-state <run-id>' }
      try {
        const state = runtime.state(invocation.agent, args[0])
        const variants = Object.values(state.variants)
          .map((variant) => `${variant.variantId}:${variant.status}`)
          .join(' ')
        return { kind: 'success', text: `run ${args[0]} is ${state.status} (${variants})` }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })

  ctx.commands.register({
    name: 'censorfs-policy',
    description: 'update the policy directive of a running variant without invoking the model (FUSE mode only)',
    input: { hint: '<run-id> <variant-id> <directive>' },
    async handler(invocation) {
      const parts = invocation.rawInput.trim().split(/\s+/u)
      if (parts.length < 3) return { kind: 'error', text: 'Usage: /censorfs-policy <run-id> <variant-id> <directive>' }
      const [runId, variantId, ...rest] = parts
      const directive = rest.join(' ')
      try {
        const result = await runtime.updatePolicy(invocation.agent, runId, variantId, directive)
        return { kind: 'success', text: `Policy update sent to ${result.variantId} in ${result.runId}: ${result.directive}` }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })

  // 新命令：查看分支图
  ctx.commands.register({
    name: 'branch-graph',
    description: 'show the branch exploration graph — all runs, variants, and tracked subagents (--tree for the subagent hierarchy)',
    input: { hint: '[run-id] [--tree]' },
    handler(invocation) {
      try {
        const args = splitArgs(invocation.rawInput)
        if (args.includes('--tree')) {
          const tree = runtime.subagentTree()
          if (tree.nodes.length === 0) {
            return { kind: 'success', text: 'No tracked subagents in this process yet.' }
          }
          return { kind: 'success', text: formatSubagentTree(tree) }
        }
        if (args.length === 0) {
          // 列出所有探索 + 全局子代理
          // rc.2 的 Session 用 snapshotEvents() 取事件快照（不再有 events 数组属性）。
          const events = invocation.agent.session.snapshotEvents()
          const runIds = new Set()
          for (const event of events) {
            if (event?.type === 'exploration-started') runIds.add(event.data.runId)
          }
          const subagents = runtime.subagentOverview()
          const lines = []
          if (runIds.size > 0) {
            lines.push('Branch explorations:')
            for (const id of runIds) lines.push(`  ${id}`)
          }
          if (subagents.length > 0) {
            if (lines.length > 0) lines.push('')
            lines.push('Tracked subagents:')
            for (const sa of subagents) {
              const status = sa.endedAt !== undefined ? `ended (${sa.stopReason})` : 'running'
              const lastActivity = sa.activities.at(-1)
              const activityStr = lastActivity !== undefined
                ? ` last: ${lastActivity.type}${lastActivity.tool !== undefined ? ` ${lastActivity.tool}` : ''}`
                : ''
              lines.push(`  [${status}] ${sa.sessionId} (${sa.provider})${activityStr}`)
            }
          }
          if (lines.length === 0) return { kind: 'success', text: 'No branch explorations or tracked subagents.' }
          lines.push('')
          lines.push('Use /branch-graph <run-id> for exploration details, /branch-graph --tree for the subagent hierarchy.')
          return { kind: 'success', text: lines.join('\n') }
        }
        const state = runtime.state(invocation.agent, args[0])
        return {
          kind: 'success',
          text: formatBranchGraph(state),
        }
      } catch (error) {
        return { kind: 'error', text: error instanceof Error ? error.message : String(error) }
      }
    },
  })
}

// 渲染 /branch-graph --tree 的子代理层级树
function formatSubagentTree(tree) {
  const lines = ['Subagent tree (root: main session)', '']
  const render = (key, depth) => {
    const bucket = tree.children.get(key)
    if (bucket === undefined) return
    for (const node of bucket) {
      const indent = '  '.repeat(depth + 1)
      const time = new Date(node.startedAt).toISOString().slice(11, 19)
      const end = node.endedAt === undefined ? '' : ` → ${new Date(node.endedAt).toISOString().slice(11, 19)}`
      const variant = node.variantId !== undefined ? ` [explore/${node.variantId}]` : ''
      lines.push(`${indent}- [${node.status}] ${node.label}${variant} (${time}${end}, ${node.activityCount} activities)`)
      render(node.sessionId, depth + 1)
    }
  }
  render('main', 0)
  return lines.join('\n')
}

function formatBranchGraph(state) {
  const lines = []
  lines.push(`Branch Exploration: ${state.runId}`)
  lines.push(`Task: ${state.task}`)
  lines.push('Mode: FUSE isolated')
  lines.push(`Branch: ${state.branch} @ ${state.expectedHead.generation_id}/${state.expectedHead.head_seq}`)
  lines.push(`Status: ${state.status}`)
  lines.push('')
  const variants = Object.values(state.variants)
  for (const variant of variants) {
    const icon = ({
      running: '...',
      prepared: 'OK',
      'validation-failed': '!!',
      failed: 'XX',
      published: '==>',
      aborted: 'xx',
      stale: '??',
    })[variant.status] ?? '??'
    lines.push(`  ${icon} ${variant.variantId} [${variant.status}] ${variant.label}`)
    if (variant.childSessionId) {
      lines.push(`     child session: ${variant.childSessionId}`)
    }
    if (variant.lastActivity) {
      lines.push(`     last activity: ${variant.lastActivity.type} ${variant.lastActivity.tool ?? ''}`)
    }
    if (variant.candidate) {
      lines.push(`     candidate: ${variant.candidate.candidate_id}`)
    }
    if (variant.error) {
      lines.push(`     error: ${variant.error}`)
    }
  }
  if (state.ranking.length > 0) {
    lines.push('')
    lines.push('Ranking:')
    for (const entry of state.ranking) {
      lines.push(`  #${entry.rank} ${entry.variantId} (${entry.changedPaths} paths, ${(entry.durationMs / 1000).toFixed(1)}s)`)
    }
  }
  return lines.join('\n')
}

function registerTool(ctx, runtime) {
  ctx.tools.register(defineTool({
    name: 'branch_explore',
    description: 'Run 2–4 materially different implementation strategies as isolated CensorFS exploration worlds. Each worker edits and tests real files in its own FUSE view; successful work freezes into immutable Candidates.',
    parameters: {
      task: { type: 'string', required: true, description: 'The user task shared by every world.' },
      strategies: {
        type: 'array',
        required: true,
        description: 'Two to four deliberately distinct implementation strategies.',
        items: {
          type: 'object',
          additionalProperties: false,
          properties: {
            id: { type: 'string', required: true, description: 'Stable short variant id.' },
            label: { type: 'string', required: true, description: 'Human-readable strategy title.' },
            instruction: { type: 'string', required: true, description: 'Concrete approach that differs materially from the others.' },
          },
        },
      },
      validationProfile: {
        type: 'string',
        description:
          'Optional validation profile. Omit to use defaultValidationProfile when configured, otherwise skip validation.',
      },
    },
    output: {
      schema: { type: 'object', additionalProperties: true },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    async execute(args, exec) {
      if (exec.agent === undefined) throw new Error('branch_explore requires a calling Agent')
      return runtime.explore(exec.agent, args, exec.signal)
    },
    presentCall: () => ({ card: 'generic', title: 'Branch Exploration' }),
    presentResult: () => ({ card: 'generic' }),
  }))
}

function registerInProcessTool(ctx, runtime) {
  ctx.tools.register(defineTool({
    name: 'branch_explore_inprocess',
    description: 'Run 2–4 implementation strategies as in-process Agents whose file and shell tools execute in dedicated CensorFS mount-namespace Runners. Successful work freezes into immutable Candidates.',
    parameters: {
      task: { type: 'string', required: true, description: 'The user task shared by every world.' },
      strategies: {
        type: 'array',
        required: true,
        description: 'Two to four deliberately distinct implementation strategies.',
        items: {
          type: 'object',
          additionalProperties: false,
          properties: {
            id: { type: 'string', required: true, description: 'Stable short variant id.' },
            label: { type: 'string', required: true, description: 'Human-readable strategy title.' },
            instruction: { type: 'string', required: true, description: 'Concrete approach that differs materially from the others.' },
          },
        },
      },
      validationProfile: {
        type: 'string',
        description:
          'Optional validation profile. Omit to use defaultValidationProfile when configured, otherwise skip validation.',
      },
    },
    output: {
      schema: { type: 'object', additionalProperties: true },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    async execute(args, exec) {
      if (exec.agent === undefined) throw new Error('branch_explore_inprocess requires a calling Agent')
      return runtime.exploreInProcess(exec.agent, args, exec.signal)
    },
    presentCall: () => ({ card: 'generic', title: 'Branch Exploration · In Process' }),
    presentResult: () => ({ card: 'generic' }),
  }))
}

export async function apply(ctx, rawConfig) {
  const config = normalizeConfig(rawConfig)
  registerEventTypes()

  const cli = new CensorFsCli({
    command: config.censorfsCommand,
    socket: config.socket,
    cwd: config.controlPlaneCwd,
  })

  // ── Track construction: build ONLY what this deployment enables. ──
  // A disabled track must not be constructed, registered, or advertised; the
  // combined runtime receives `undefined` for it and fails closed on
  // cross-track calls with a track-naming error.
  const tracks = resolveEnabledTracks(config)
  let fuseProvider
  let fuseRuntime
  if (tracks.fuse) {
    fuseProvider = new FuseProvider(config.providerName, config, ctx.logger)
    fuseRuntime = new BranchExploreRuntime(ctx, config, cli, fuseProvider)
  }

  let runnerManager
  let inProcessProvider
  let namespaceRuntime
  if (tracks.runner) {
    const environment = await probeRunnerEnvironment(config, process.env)
    config.runnerIsolation.cgroupEnabled = environment.cgroupEnabled === true
    config.runnerIsolation.environment = environment
    if (environment.error !== undefined) {
      ctx.logger?.error?.(`censorfs isolation failed closed: ${environment.error}`)
    } else {
      ctx.logger?.info?.(`censorfs isolation detected: mode=${environment.requestedMode} minimumLevel=${environment.minimumLevel} effective=${environment.effectiveLevel} cgroupEnabled=${environment.cgroupEnabled} controllers=${(environment.controllers ?? []).join(',') || '-'} daemon=${environment.daemon?.available === true ? (environment.daemon.live === true ? 'live' : 'stale-socket') : 'missing'} fuse=${environment.fuse} bwrap=${environment.commands?.bwrap ?? 'missing'}`)
      for (const warning of environment.warnings ?? []) ctx.logger?.warn?.(`censorfs isolation warning: ${warning}`)
    }
    runnerManager = new RunnerManager(config, ctx.logger)
    inProcessProvider = new InProcessCensorFsProvider(config.inProcessProviderName, runnerManager)
    // `fuseProvider` is `undefined` in fuse-only deployments; the runner
    // runtime only touches it on its (combined-unrouted) external path.
    namespaceRuntime = new ParallelWorldsRuntime(ctx, config, cli, fuseProvider, inProcessProvider, runnerManager)
  }

  const runtime = new CombinedBranchExploreRuntime(fuseRuntime, namespaceRuntime, config)
  ctx.provide('branch-explore', runtime)
  ctx.effect(() => async () => {
    await runtime.dispose()
    await runnerManager?.dispose()
  }, 'censorfs-parallel-worlds.dispose')
  if (fuseProvider !== undefined) ctx.subagents.registerProvider(fuseProvider)
  if (inProcessProvider !== undefined) ctx.subagents.registerProvider(inProcessProvider)
  if (runnerManager !== undefined) registerRunnerToolProxy(ctx, runnerManager)
  if (fuseRuntime !== undefined) registerTool(ctx, runtime)
  if (namespaceRuntime !== undefined) registerInProcessTool(ctx, runtime)
  registerCommands(ctx, runtime, config)

  // 全局子代理活动追踪：捕捉所有子代理的启动、结束和活动，
  // 不限于 branch_explore 工具启动的子代理。
  // 使用 { global: true } 绕过 scope 过滤，接收所有事件。
  ctx.on('subagent/start', (info) => {
    runtime.trackSubagentStart(info)
  }, { global: true })

  ctx.on('subagent/end', (info) => {
    runtime.trackSubagentEnd(info)
  }, { global: true })

  // 监听所有 session 事件，为追踪中的子代理记录活动摘要
  ctx.on('session/event', (session, event) => {
    runtime.trackSubagentActivity(session, event)
  }, { global: true })
}

// 同时支持命名导出和 default 导出（cordis 兼容）
const plugin = { apply, name, inject }
export default plugin