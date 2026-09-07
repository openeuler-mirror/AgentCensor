import { randomUUID } from 'node:crypto'
import { spawn } from 'node:child_process'
import { createServer } from 'node:net'
import { mkdir } from 'node:fs/promises'
import { join } from 'node:path'
import { scrubbedParentEnv } from '@deepseek-ai/dsh-subprocess'
import { temporaryEnvironment } from './environment.js'
import { CensorFsCommandError } from './censorfs-cli.js'
import { foldExploration, rankVariants, ACTIVITY_EVENT_TYPES, summarizeActivityEvent, labelFromDescriptor, subagentNodeStatus, projectFuseEvent, PROCESS_STARTED_AT, reconcileInterruptedSubagents } from './events.js'
import { prepareVariantTmp, resolveValidationProfile, validateCandidate, safeSegment } from './validation.js'
import { resolveMounterCommand } from './fuse-provider.js'

function errorText(error) {
  return error instanceof Error ? error.message : String(error)
}

function outputText(blocks) {
  return blocks
    .filter((block) => block?.type === 'text')
    .map((block) => block.text)
    .join('')
}

function validateStrategies(strategies) {
  if (!Array.isArray(strategies) || strategies.length < 2 || strategies.length > 4) {
    throw new Error('branch_explore requires 2–4 strategies')
  }
  const ids = new Set()
  return strategies.map((strategy, index) => {
    const variantId = strategy?.id ?? `variant-${index + 1}`
    if (!/^[a-zA-Z0-9_.-]{1,64}$/u.test(variantId)) {
      throw new Error(`invalid variant id: ${variantId}`)
    }
    if (ids.has(variantId)) throw new Error(`duplicate variant id: ${variantId}`)
    ids.add(variantId)
    if (typeof strategy?.label !== 'string' || strategy.label.trim() === '') {
      throw new Error(`strategy ${variantId} needs a label`)
    }
    if (typeof strategy?.instruction !== 'string' || strategy.instruction.trim() === '') {
      throw new Error(`strategy ${variantId} needs an instruction`)
    }
    return { id: variantId, label: strategy.label, instruction: strategy.instruction }
  })
}

function append(session, type, data) {
  session.append(type, JSON.parse(JSON.stringify(data)))
}

/**
 * BranchExploreRuntime — 一次分支探索的完整编排（FUSE 模式）。
 *
 * 每个 variant 一个独立 CensorFS View，写入私有 Upper/Delta，只有 prepare→Candidate→publish 才合并成新 Generation。
 */
export class BranchExploreRuntime {
  constructor(ctx, config, cli, provider) {
    this.ctx = ctx
    this.config = config
    this.cli = cli
    this.provider = provider
    this.previews = new Map()
    // 全局子代理追踪：sessionId -> { info, activities, sysActivities, startedAt, endedAt }
    this.trackedSubagents = new Map()
    // childSessionId -> 根会话 id
    this.rootBySession = new Map()
    // variant 绑定：childSessionId -> { agent, runId, variantId }
    this.variantBindings = new Map()
    // continuable 子代理（如 agent-teams）在 subagent/start 前已写 descriptor；先暂存 label，start 到达时取回
    this.pendingLabels = new Map()
    // FUSE 子进程回传的嵌套子代理 sessionId；仅辅助 subagent-start 的 parent 替换，不作身份判据
    this.fuseNestedIds = new Map()
}

  /** 隔离模式：耦合版固定为 FUSE */
  get mode() {
    return 'fuse'
  }

  // 通过 DSH 全局 subagent/start|end、session/event 事件捕捉所有子代理，持久化为 subagent-* 事件写入根会话

  // 沿 header.origin === 'subagent' 链向上溯源根会话
  resolveRootSession(sessionId) {
    let current = this.ctx.sessions.get(sessionId)
    const seen = new Set()

    while (
      current !== undefined &&
      current.header?.origin === 'subagent'
    ) {
      const parentId = current.header.parentSession

      if (
        parentId === undefined ||
        seen.has(current.id)
      ) {
        return undefined
      }

      seen.add(current.id)
      current = this.ctx.sessions.get(parentId)
    }

    return current
  }

  trackSubagentStart(info) {
    const parentSessionId =
      this.ctx.sessions.get(info.id)
        ?.header?.parentSession

    const startedAt = Date.now()

    // Variant binding 与 subagent/start 谁先到都允许。
    // in-process worker 的 binding 是 ctx.subagents.start() 返回 run.id 后补注册，
    // FUSE / 未来 provider 也可能出现相反时序。
    const binding =
      this.variantBindings.get(info.id)

    const runId =
      binding?.runId ??
      info.runId

    const variantId =
      binding?.variantId ??
      info.variantId

    const label =
      info.label ??
      binding?.label ??
      this.pendingLabels.get(info.id)

    this.pendingLabels.delete(info.id)

    this.trackedSubagents.set(info.id, {
      runId,
      variantId,
      provider: info.provider,
      sessionId: info.id,
      local: info.local,

      ...(label === undefined
        ? {}
        : { label }),

      parentSessionId,

      activities: [],
      sysActivities: [],
      startedAt,
    })

    const root =
      this.resolveRootSession(info.id)

    if (root !== undefined) {
      this.rootBySession.set(
        info.id,
        root.id,
      )

      if (root.id !== info.id) {
        if (
          !root.events.some(function (event) {
            return event.type ===
              'subagent-graph-opened'
          })
        ) {
          append(
            root,
            'subagent-graph-opened',
            {
              rootSessionId: root.id,
              openedAt: startedAt,
            },
          )
        }

        append(
          root,
          'subagent-started',
          {
            sessionId: info.id,
            parentSessionId,
            rootSessionId: root.id,
            provider: info.provider,
            local: info.local === true,
            runId,
            variantId,

            ...(label === undefined
              ? {}
              : { label }),

            startedAt,
          },
        )

        // 如果 binding 比 subagent/start 更早注册，registerVariantBinding()
        // 当时还没有 rootBySession，无法持久化 variant/binding。
        // 现在 root 已解析出来，在这里补上。
        if (binding !== undefined) {
          const activity = {
            type: 'variant/binding',
            runId: binding.runId,
            variantId: binding.variantId,
            runTask: binding.runTask,
            preview:
              binding.label !== undefined
                ? `explore variant ${binding.variantId}: ${binding.label}`
                : `explore variant ${binding.variantId}`,
            at: Date.now(),
          }

          append(root, 'subagent-activity', {
            sessionId: info.id,
            rootSessionId: root.id,
            activity,
          })
        }
      }
    }
  }

  trackSubagentEnd(info) {
    const tracked =
      this.trackedSubagents.get(info.id)

    if (tracked !== undefined) {
      tracked.stopReason =
        info.stopReason

      tracked.endedAt =
        Date.now()
    }

    const rootId =
      this.rootBySession.get(info.id)

    const root =
      rootId === undefined
        ? undefined
        : this.ctx.sessions.get(rootId)

    if (root !== undefined) {
      append(
        root,
        'subagent-ended',
        {
          sessionId: info.id,
          rootSessionId: rootId,
          stopReason: info.stopReason,
          endedAt: Date.now(),
        },
      )
    }
  }

  trackSubagentActivity(session, event) {
    const tracked =
      this.trackedSubagents.get(session.id)

    // continuable 子代理 descriptor 可能早于 subagent/start。
    if (
      event.type ===
      'subagent/descriptor'
    ) {
      const label =
        labelFromDescriptor(event.data)

      if (label === undefined) {
        return
      }

      if (tracked === undefined) {
        this.pendingLabels.set(
          session.id,
          label,
        )

        return
      }

      tracked.label = label

      const activity = {
        type: 'subagent/descriptor',
        preview: label,
        at: Date.now(),
      }

      tracked.activities.push(activity)

      this.persistSubagentActivity(
        session.id,
        activity,
      )

      return
    }

    if (tracked === undefined) {
      return
    }

    if (
      !ACTIVITY_EVENT_TYPES.has(
        event.type,
      )
    ) {
      return
    }

    const summary =
      summarizeActivityEvent(event)

    const activity = {
      ...summary,
      at: Date.now(),
    }

    tracked.activities.push(activity)

    this.persistSubagentActivity(
      session.id,
      activity,
    )
  }

  // 一条活动写入根会话：variant 绑定的写 variant-activity（探索卡片）+ subagent-activity；其余只走 subagent-activity
  persistSubagentActivity(sessionId, activity) {
    const rootId = this.rootBySession.get(sessionId)
    const root = rootId === undefined ? undefined : this.ctx.sessions.get(rootId)
    if (root !== undefined && root.id !== sessionId) {
      append(root, 'subagent-activity', { sessionId, rootSessionId: rootId, activity })
    }
    const binding = this.variantBindings.get(sessionId)
    if (binding !== undefined) {
      append(binding.agent.session, 'variant-activity', {
        runId: binding.runId,
        variantId: binding.variantId,
        activity,
      })
    }
  }

  // FUSE 模式：子进程 out-of-process，其 session 不在父进程 ctx.sessions 里，
  // resolveRootSession/rootBySession 都解析不到根会话，这里直接往根会话写图事件。
  // 单条：子进程事件落盘为 subagent-activity（分支图）+ variant-activity（卡片）。
  relayFuseEvent(agent, childSessionId, runId, variantId, raw) {
    const projected = projectFuseEvent(
      agent.session.id,
      childSessionId,
      this.fuseNestedIds.get(childSessionId) ?? this.acquireFuseNested(childSessionId),
      { ...raw, runId, variantId },
    )
    if (projected === null) return
    for (const { type, data } of projected) append(agent.session, type, data)
  }

  // 取（或惰性建）某 Variant 的嵌套 id 集合。每 Variant 独立一套，隔离 A/B Variant。
  acquireFuseNested(childSessionId) {
    let set = this.fuseNestedIds.get(childSessionId)
    if (set === undefined) {
      set = new Set()
      this.fuseNestedIds.set(childSessionId, set)
    }
    return set
  }

  // 系统级活动注入点（eBPF/auditd 接缝）
  injectSysActivity(sessionId, sysActivity) {
    const tracked =
      this.trackedSubagents.get(sessionId)

    if (tracked === undefined) return false

    const activity = {
      ...sysActivity,
      at: sysActivity.at ?? Date.now(),
    }

    tracked.sysActivities.push(activity)

    // 全局 Subagent Graph 始终要一份
    const rootId =
      this.rootBySession.get(sessionId)

    const root =
      rootId === undefined
        ? undefined
        : this.ctx.sessions.get(rootId)

    if (
      root !== undefined &&
      root.id !== sessionId
    ) {
      append(root, 'subagent-activity', {
        sessionId,
        rootSessionId: rootId,
        activity,
        sys: true,
      })
    }

    // 若同时属于 branch_explore，Explore Card 再得到一份 projection。
    const binding =
      this.variantBindings.get(sessionId)

    if (binding !== undefined) {
      append(
        binding.agent.session,
        'variant-sys-activity',
        {
          runId: binding.runId,
          variantId: binding.variantId,
          activity,
        },
      )
    }

    return true
  }

  // 注册 variant 与子代理 session 的绑定，并写一条合成活动让全局图按 run 分组（task lane）
  registerVariantBinding(childSessionId, agent, runId, variantId, label, runTask) {
    this.variantBindings.set(childSessionId, { agent, runId, variantId, label, runTask })
    const tracked = this.trackedSubagents.get(childSessionId)
    if (tracked !== undefined) {
      tracked.runId = runId
      tracked.variantId = variantId

      if (
        tracked.label === undefined &&
        label !== undefined
      ) {
        tracked.label = label
      }
    }
    const rootId = this.rootBySession.get(childSessionId)
    const root = rootId === undefined ? undefined : this.ctx.sessions.get(rootId)
    if (root !== undefined && root.id !== childSessionId) {
      const activity = {
        type: 'variant/binding',
        runId,
        variantId,
        runTask,
        preview: label !== undefined ? `explore variant ${variantId}: ${label}` : `explore variant ${variantId}`,
        at: Date.now(),
      }
      append(root, 'subagent-activity', { sessionId: childSessionId, rootSessionId: rootId, activity })
    }
  }

  unregisterVariantBinding(childSessionId) {
    this.variantBindings.delete(childSessionId)
    this.fuseNestedIds.delete(childSessionId)
  }

  // 返回全部追踪中的子代理状态（供前端分支图展示）
  subagentOverview() {
    return [...this.trackedSubagents.values()]
  }

  // 清空某根会话的全局子代理图：追加标记事件让前端重置
  clearSubagentGraph(sessionId) {
    const root = this.resolveRootSession(sessionId) ?? this.ctx.sessions.get(sessionId)
    if (root === undefined) return false
    append(root, 'subagent-graph-cleared', { rootSessionId: root.id, clearedAt: Date.now() })
    const rootId = root.id
    for (const [childId, owner] of this.rootBySession) {
      if (owner === rootId) {
        this.trackedSubagents.delete(childId)
        this.rootBySession.delete(childId)
        this.variantBindings.delete(childId)
        this.pendingLabels.delete(childId)
        this.fuseNestedIds.delete(childId)
      }
    }
    return true
  }

  // 子代理层级树（/branch-graph --tree 用）：
  // { root: 'main', children: { parentKey: [node, ...] }, nodes: [node, ...] }
  subagentTree() {
    const nodes = [...this.trackedSubagents.values()].map((record) => ({
      sessionId: record.sessionId,
      parentSessionId: record.parentSessionId,
      label: record.label ?? `${record.provider ?? 'subagent'}:${record.sessionId.slice(0, 8)}`,
      provider: record.provider,
      local: record.local,
      runId: record.runId,
      variantId: record.variantId,
      status: record.endedAt === undefined ? 'running' : subagentNodeStatus(record.stopReason),
      activityCount: record.activities.length + record.sysActivities.length,
      startedAt: record.startedAt,
      endedAt: record.endedAt,
    }))
    const children = new Map()
    for (const node of nodes) {
      const key = node.parentSessionId ?? 'main'
      const bucket = children.get(key) ?? []
      bucket.push(node)
      children.set(key, bucket)
    }
    for (const bucket of children.values()) bucket.sort((a, b) => a.startedAt - b.startedAt)
    return { root: 'main', children, nodes }
  }

  async explore(agent, input, signal) {
    const strategies = validateStrategies(input.strategies)
    const validationProfile =
      input.validationProfile ??
      this.config.defaultValidationProfile ??
      null
    const checks =
      validationProfile === null
        ? []
        : resolveValidationProfile(this.config.validationProfiles, validationProfile)
    const previewProfile =
      input.previewProfile ??
      this.config.defaultPreviewProfile ??
      null

    // FUSE 模式读 CensorFS Head 作为基线
    const expectedHead = await this.cli.head(this.config.branch)

    const runId = `explore-${randomUUID()}`
    const started = Date.now()
    append(agent.session, 'exploration-started', {
      runId,
      task: input.task,
      branch: this.config.branch,
      fsEnabled: true,
      mode: this.mode,
      expectedHead,
      validationProfile,
      previewProfile,
      strategies,
      startedAt: started,
    })

const settled = await Promise.all(strategies.map((strategy) =>
      this.runVariant(agent, runId, input.task, strategy, expectedHead, checks, validationProfile, signal)))

    const ranking = rankVariants(settled)
    append(agent.session, 'ranking-ready', { runId, ranking, createdAt: Date.now() })
    append(agent.session, 'exploration-ended', {
      runId,
      status: signal.aborted ? 'cancelled' : 'ranked',
      durationMs: Date.now() - started,
      prepared: settled.filter((variant) => variant.candidate !== undefined).length,
      failed: settled.filter((variant) => variant.candidate === undefined).length,
    })
    await this.ctx.sessions.flush(agent.session)
    return {
      runId,
      expectedHead,
      ranking,
      variants: settled.map((variant) => ({
        variantId: variant.variantId,
        label: variant.label,
        childSessionId: variant.childSessionId,
        candidateId: variant.candidate?.candidate_id ?? null,
        generationId: variant.generation?.generation_id ?? null,
        requiredPassed: variant.validation?.requiredPassed ?? null,
        validation: variant.validation?.checks?.map((check) => ({
          name: check.name,
          required: check.required,
          passed: check.passed,
          durationMs: check.durationMs,
        })) ?? [],
        changedPaths: variant.pathDiff?.map((diff) => diff.path) ?? [],
        summary: variant.result?.summary ?? '',
        error: variant.error ?? null,
      })),
    }
  }

  async runVariant(agent, runId, task, strategy, expectedHead, checks, validationProfile, signal) {
    const started = Date.now()
    let opened
    let prepared
    let tmpDir
    let childSessionId
    let worker
    let stopReason = 'error'

    try {
      // FUSE：调 cli.open() 创建 Ticket + View
      opened = await this.cli.open({
        branch: this.config.branch,
        expectedHead,
        runId,
        variantId: strategy.id,
      })
      tmpDir = await prepareVariantTmp(
        this.config.tmpRoot,
        runId,
        strategy.id,
        opened.view.owner_uid,
        opened.view.owner_gid,
      )

      const workspaceRule = [
        'Complete the task directly in /workspace. Read files only as needed to make the change, then modify real files and run useful checks.',
        'Your /workspace is an isolated CensorFS view — other variants cannot see your changes until published.',
        'Do not start development servers, preview servers, watch processes, GUI loops, or other commands that wait indefinitely. Use only finite checks during implementation. Preview is started separately from the frozen Candidate.',
      ].join(' ')

      const workerPrompt = [{
        type: 'text',
        text: [
          `Task: ${task}`,
          `Your distinct strategy: ${strategy.label} — ${strategy.instruction}`,
          workspaceRule,
        ].join('\n\n'),
      }]

      const run = await this.provider.withBinding({
        runId,
        variantId: strategy.id,
        viewId: opened.view.view_id,
        uid: opened.view.owner_uid,
        gid: opened.view.owner_gid,
        tmpDir,
        // 实时事件回调：子进程跑动中每条事件到达即落盘，面板/图流式更新。
        onEvent: (raw, providerSessionId) => this.relayFuseEvent(agent, providerSessionId, runId, strategy.id, raw),
      }, () => this.ctx.subagents.start(this.provider.name, {
        label: strategy.label,
        prompt: workerPrompt,
        parent: agent,
        signal,
      }))

      childSessionId = run.id
      // 注册 variant 绑定，使全局活动追踪能持久化为 variant-activity 事件
      this.registerVariantBinding(childSessionId, agent, runId, strategy.id, strategy.label, task)

      // 子进程 out-of-process，其 session 不在父进程 ctx.sessions 里，直接往根会话写图事件。
      // 图视图（client）依赖 subagent-graph-opened 作为一次性起点。
      if (!agent.session.events.some((e) => e.type === 'subagent-graph-opened')) {
        append(agent.session, 'subagent-graph-opened', {
          rootSessionId: agent.session.id,
          openedAt: started,
        })
      }
      append(agent.session, 'subagent-started', {
        sessionId: childSessionId,
        parentSessionId: agent.session.id,
        rootSessionId: agent.session.id,
        provider: this.provider.name,
        local: false,
        runId,
        variantId: strategy.id,
        label: strategy.label,
        startedAt: started,
      })

      // variant-running 在子会话创建后落盘：携带 childSessionId（README 契约）
      append(agent.session, 'variant-running', {
        runId,
        variantId: strategy.id,
        label: strategy.label,
        strategy: strategy.instruction,
        fsEnabled: true,
        mode: this.mode,
        childSessionId,
        ticketId: opened.ticket.ticket_id,
        txId: opened.tx.tx_id,
        viewId: opened.view.view_id,
        startedAt: started,
      })
      try {
        worker = await run.result
        stopReason = worker?.stopReason ?? 'completed'
        // 事件已由 binding.onEvent 实时落盘，这里不再批量 relay（避免重复）。
      } finally {
        try {
          await run.dispose()
        } catch (error) {
          this.ctx.logger?.warn?.(`worker dispose failed: ${errorText(error)}`)
        }
        this.unregisterVariantBinding(childSessionId)
        append(agent.session, 'subagent-ended', {
          sessionId: childSessionId,
          rootSessionId: agent.session.id,
          stopReason,
          endedAt: Date.now(),
        })
      }

      if (stopReason !== 'completed') {
        throw new Error(`worker stopped with ${stopReason}: ${outputText(worker?.output ?? [])}`)
      }
      const summary = outputText(worker.output)

      // FUSE：调 cli.prepare() 冻结 Candidate
      prepared = await this.cli.prepare({
        ticketId: opened.ticket.ticket_id,
        viewId: opened.view.view_id,
        runId,
        variantId: strategy.id,
      })

// FUSE：在 Candidate 只读 View 里跑验证
      let validation

      if (checks.length === 0) {
        validation = {
          status: 'unvalidated',
          profile: null,
          checks: [],
          requiredPassed: null,
          passed: null,
        }
      } else {
        const result = await validateCandidate({
          cli: this.cli,
          candidateId: prepared.candidate.candidate_id,
          checks,
          mounterCommand: resolveMounterCommand(this.config),
          socket: this.config.socket,
          controlPlaneCwd: this.config.controlPlaneCwd,
          childEnv: this.config.childEnv,
          tmpDir,
          signal,
        })

        validation = {
          ...result,
          status: result.requiredPassed ? 'passed' : 'failed',
          profile: validationProfile,
        }
      }

      const result = {
        runId,
        variantId: strategy.id,
        label: strategy.label,
        strategy: strategy.instruction,
        childSessionId,
        ticketId: opened.ticket.ticket_id,
        txId: opened.tx.tx_id,
        candidate: prepared.candidate,
        generation: prepared.generation,
        pathDiff: prepared.path_diff ?? prepared.pathDiff,
        textDiff: prepared.text_diff ?? prepared.textDiff,
        validation,
        result: {
          summary,
          output: worker.output,
          stopReason: 'completed',
        },
        durationMs: Date.now() - started,
      }
      append(agent.session, 'variant-prepared', result)
      return result
    } catch (error) {
      // FUSE：调 cli.abort() 放弃 Ticket
      if (opened !== undefined && prepared === undefined) {
        await this.cli.abort({
          ticketId: opened.ticket.ticket_id,
          viewId: opened.view.view_id,
          runId,
          variantId: strategy.id,
        }).catch((abortError) => this.ctx.logger.warn(
          `could not abort ${runId}/${strategy.id}: ${errorText(abortError)}`,
        ))
      }
      const result = {
        runId,
        variantId: strategy.id,
        label: strategy.label,
        strategy: strategy.instruction,
        childSessionId,
        ...(opened === undefined ? {} : {
          ticketId: opened.ticket.ticket_id,
          txId: opened.tx.tx_id,
        }),
        ...(prepared === undefined ? {} : {
          candidate: prepared.candidate,
          generation: prepared.generation,
          pathDiff: prepared.path_diff ?? prepared.pathDiff,
          textDiff: prepared.text_diff ?? prepared.textDiff,
        }),
        error: errorText(error),
        result: {
          summary: errorText(error),
          output: worker?.output ?? [],
          stopReason: worker?.stopReason ?? 'error',
        },
        durationMs: Date.now() - started,
      }
      // 被 abort 的 worker（stopReason 'aborted'）落 variant-aborted 而非
      // variant-failed：卡片已被 abortVariant 标成 aborted，不能被 failed 覆盖。
      append(agent.session,
        worker?.stopReason === 'aborted' ? 'variant-aborted' : 'variant-failed',
        result)
      return result
    }
  }

  state(agent, runId) {
    // 重启残留 reconcile（幂等）：上一进程启动的 run，其 worker 已随进程死亡，
    // 但 terminal 事件没落盘 —— 卡片/树图会永远定格 running。这里在读取时补写。
    const reconciledNodes = reconcileInterruptedSubagents(agent.session)
    let state = foldExploration(agent.session.events, runId)
    if (state === undefined) throw new Error(`exploration ${runId} does not belong to this session`)
    let reconciledVariants = 0
    if ((state.startedAt ?? 0) < PROCESS_STARTED_AT) {
      const staleVariants = Object.values(state.variants).filter((variant) => variant.status === 'running')
      for (const variant of staleVariants) {
        append(agent.session, 'variant-failed', {
          runId,
          variantId: variant.variantId,
          label: variant.label,
          error: 'worker interrupted: server restarted',
          result: { summary: 'worker interrupted: server restarted', output: [], stopReason: 'error' },
        })
      }
      reconciledVariants = staleVariants.length
      if (reconciledVariants > 0) state = foldExploration(agent.session.events, runId)
    }
    // 补写的事件要落盘（fire-and-forget），否则下次重启后磁盘上仍是旧状态。
    if (reconciledNodes + reconciledVariants > 0) this.ctx.sessions.flush(agent.session).catch(() => {})
    return state
  }

  // 对已 prepared 的 Candidate 重新验证（用户显式选 profile 或走 default）。
  // 未选 profile 且无 default 时返回 unvalidated，不跑任何验证命令、不报错。
  async validate(agent, runId, variantId, profile) {
    const state = this.state(agent, runId)
    const variant = state.variants[variantId]
    if (variant?.candidate === undefined) throw new Error(`variant ${variantId} has no Candidate`)

    const requested = profile ?? this.config.defaultValidationProfile
    if (typeof requested !== 'string' || requested.length === 0) {
      const validation = {
        status: 'unvalidated',
        profile: null,
        checks: [],
        requiredPassed: null,
        passed: null,
      }
      append(agent.session, 'variant-validated', { runId, variantId, validation, validatedAt: Date.now() })
      return validation
    }

    const checks = resolveValidationProfile(this.config.validationProfiles, requested)
    const tmpDir = join(this.config.tmpRoot, safeSegment(runId), safeSegment(variantId))
    await mkdir(tmpDir, { recursive: true, mode: 0o700 })
    const result = await validateCandidate({
      cli: this.cli,
      candidateId: variant.candidate.candidate_id,
      checks,
      mounterCommand: resolveMounterCommand(this.config),
      socket: this.config.socket,
      controlPlaneCwd: this.config.controlPlaneCwd,
      childEnv: this.config.childEnv,
      tmpDir,
      signal: undefined,
    })
    const validation = { ...result, status: result.requiredPassed ? 'passed' : 'failed', profile: requested }
    append(agent.session, 'variant-validated', { runId, variantId, validation, validatedAt: Date.now() })
    await this.ctx.sessions.flush(agent.session)
    return validation
  }

  // FUSE：发布走 CAS Publish（含 stale 检查）
  async publish(agent, runId, variantId, force = false) {
    const state = this.state(agent, runId)
    const variant = state.variants[variantId]
    if (variant?.candidate === undefined) throw new Error(`variant ${variantId} has no Candidate`)
    if (variant.validation?.status === 'failed' && !force) {
      throw new Error('required validation did not pass; repeat adoption with --force after explicit confirmation')
    }
    const decisionId = `session:${agent.session.id}:run:${runId}:variant:${variantId}`
    try {
      const result = await this.cli.publish({
        candidateId: variant.candidate.candidate_id,
        expectedHead: state.expectedHead,
        decisionId,
        runId,
        variantId,
      })
      append(agent.session, 'variant-published', {
        runId,
        variantId,
        candidateId: variant.candidate.candidate_id,
        generationId: result.receipt.new_generation,
        receipt: result.receipt,
        publishedAt: Date.now(),
      })
      for (const loser of Object.values(state.variants)) {
        if (loser.variantId === variantId) continue
        if (loser.ticketId === undefined) continue
        await this.abortVariant(agent, state, loser).catch((error) => this.ctx.logger.warn(
          `could not abort losing variant ${loser.variantId}: ${errorText(error)}`,
        ))
      }
      await this.ctx.sessions.flush(agent.session)
      return result
    } catch (error) {
      if (error instanceof CensorFsCommandError && error.stale) {
        append(agent.session, 'variant-stale', {
          runId,
          variantId,
          candidateId: variant.candidate.candidate_id,
          reason: error.message,
          detectedAt: Date.now(),
        })
        await this.ctx.sessions.flush(agent.session)
      }
      throw error
    }
  }

  async abortVariant(agent, state, variant) {
    if (variant.ticketId === undefined) return undefined
    // 先真正停止 worker（cancel → 3s SIGTERM 升级），再释放 daemon ticket。
    // 只 abort ticket 会让 worker 继续跑：卡片 aborted 而代理图一直 RUNNING。
    // worker 死后 runVariant 的 finally 会落 subagent-ended，树图随之收敛。
    this.provider.cancel(state.runId, variant.variantId)
    const result = await this.cli.abort({
      ticketId: variant.ticketId,
      viewId: variant.viewId,
      runId: state.runId,
      variantId: variant.variantId,
    })
    // running worker 的 aborted 由 runVariant 在 worker 真正 settle（stopReason=aborted）
    // 后落盘，避免卡片先显示 ABORTED 而执行图仍 RUNNING；非 running（worker 已 settle）
    // 无 runVariant 可写，这里落盘。
    if (variant.status !== 'running') {
      append(agent.session, 'variant-aborted', {
        runId: state.runId,
        variantId: variant.variantId,
        ticketId: variant.ticketId,
        candidateId: variant.candidate?.candidate_id,
        abortedAt: Date.now(),
      })
    }
    return result
  }

  async abort(agent, runId, variantId) {
    const state = this.state(agent, runId)
    const targets = variantId === undefined
      ? Object.values(state.variants)
      : [state.variants[variantId]]
    if (targets.some((variant) => variant === undefined)) throw new Error(`unknown variant ${variantId}`)
    for (const variant of targets) {
      if (!['published', 'aborted'].includes(variant.status)) await this.abortVariant(agent, state, variant)
    }
    await this.ctx.sessions.flush(agent.session)
  }

  // FUSE 模式：向运行中的 variant 发 update-policy 控制消息（运行中改策略指令）。
  // 子进程侧 exporter 收到后经 agent.steer 注入，令 worker 立即改向。
  async updatePolicy(agent, runId, variantId, directive) {
    if (!this.config.fsEnabled) throw new Error('update-policy is only available in FUSE mode')
    if (typeof directive !== 'string' || directive.trim() === '') throw new Error('policy directive is required')
    const trimmed = directive.trim()
    const sent = this.provider.sendControl?.(runId, variantId, 'update-policy', { policy: { directive: trimmed } })
    if (sent !== true) throw new Error(`no running variant ${variantId} in run ${runId} (its control channel is closed)`)
    append(agent.session, 'variant-activity', {
      runId,
      variantId,
      activity: { type: 'variant/policy', preview: trimmed, at: Date.now() },
    })
    await this.ctx.sessions.flush(agent.session)
    return { runId, variantId, directive: trimmed }
  }

  async preview(agent, runId, variantId) {
    const state = this.state(agent, runId)
    const variant = state.variants[variantId]
    if (variant?.candidate === undefined) throw new Error(`variant ${variantId} has no Candidate`)
    const profile = this.config.previewProfiles[state.previewProfile]
    if (profile === undefined) {
      return {
        candidateId: variant.candidate.candidate_id,
        generationId: variant.generation.generation_id,
      }
    }
    if (typeof profile.command !== 'string' || !Array.isArray(profile.args)
      || !profile.args.every((arg) => typeof arg === 'string')) {
      throw new Error(`preview profile ${state.previewProfile} is invalid`)
    }
    const port = await reservePort()
    const view = await this.cli.openCandidate(variant.candidate.candidate_id)
    const tmpDir = await prepareVariantTmp(
      this.config.tmpRoot,
      runId,
      `${variantId}-preview`,
      view.owner_uid,
      view.owner_gid,
    )
    const key = `${agent.session.id}:${runId}:${variantId}`
    await this.stopPreview(key)
    const child = spawn(resolveMounterCommand(this.config), [
      '--socket', this.config.socket,
      '--view-id', view.view_id,
      '--uid', String(view.owner_uid),
      '--gid', String(view.owner_gid),
      '--read-only',
      '--',
      profile.command,
      ...profile.args.map((arg) => arg.replaceAll('{port}', String(port))),
    ], {
      cwd: this.config.controlPlaneCwd,
      env: {
        ...scrubbedParentEnv(),
        ...this.config.childEnv,
        ...temporaryEnvironment(tmpDir),
      },
      stdio: 'ignore',
    })
    const record = { child, viewId: view.view_id, timer: undefined }
    this.previews.set(key, record)
    child.once('exit', () => {
      if (this.previews.get(key) === record) this.previews.delete(key)
      void this.cli.closeView(view.view_id).catch(() => undefined)
    })
    const ttlMs = Number.isSafeInteger(profile.ttlMs) ? profile.ttlMs : 600000
    record.timer = setTimeout(() => { void this.stopPreview(key) }, ttlMs)
    record.timer.unref?.()
    await new Promise((resolve) => setTimeout(resolve, 150))
    if (child.exitCode !== null) {
      await this.stopPreview(key)
      throw new Error('preview process exited during startup')
    }
    return {
      candidateId: variant.candidate.candidate_id,
      generationId: variant.generation.generation_id,
      url: `http://127.0.0.1:${port}/`,
      expiresInMs: ttlMs,
    }
  }

  async stopPreview(key) {
    const record = this.previews.get(key)
    if (record === undefined) return
    this.previews.delete(key)
    if (record.timer !== undefined) clearTimeout(record.timer)
    record.child.kill('SIGTERM')
    await this.cli.closeView(record.viewId).catch(() => undefined)
  }

  async dispose() {
    await Promise.all([...this.previews.keys()].map((key) => this.stopPreview(key)))
    this.trackedSubagents.clear()
    this.variantBindings.clear()
    this.rootBySession.clear()
  }
}

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = createServer()
    server.once('error', reject)
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      const port = typeof address === 'object' && address !== null ? address.port : undefined
      server.close((error) => {
        if (error !== undefined) reject(error)
        else if (port === undefined) reject(new Error('could not allocate preview port'))
        else resolve(port)
      })
    })
  })
}