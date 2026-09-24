import { randomUUID } from 'node:crypto'
import { spawn } from 'node:child_process'
import { createServer } from 'node:net'
import { mkdir } from 'node:fs/promises'
import { join } from 'node:path'
import { scrubbedParentEnv } from '@deepseek-ai/dsh-subprocess'
import { temporaryEnvironment } from './environment.js'
import { CensorFsCommandError } from './censorfs-cli.js'
import { detectRunnerEnvironment, probeRunnerEnvironment } from './runner-environment.js'
import { foldExploration, rankVariants, ACTIVITY_EVENT_TYPES, summarizeActivityEvent, PROCESS_STARTED_AT, reconcileInterruptedSubagents } from './events.js'
import { prepareVariantTmp, resolveValidationProfile, validateCandidate, safeSegment } from './validation.js'
import { losslessJson } from './lossless-json.js'

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
    throw new Error('branch_explore requires 2-4 strategies')
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
  session.append(type, losslessJson(data))
}

export class ParallelWorldsRuntime {
  constructor(ctx, config, cli, provider, inProcessProvider, runnerManager) {
    this.ctx = ctx
    this.config = config
    this.cli = cli
    this.provider = provider
    this.inProcessProvider = inProcessProvider
    this.runnerManager = runnerManager
    this.previews = new Map()
    this.trackedSubagents = new Map()
    this.variantBindings = new Map()
  }

  explore(agent, input, signal) {
    return this.exploreWithMode(agent, input, signal, 'external')
  }

  exploreInProcess(agent, input, signal) {
    return this.exploreWithMode(agent, input, signal, 'in-process')
  }

  async exploreWithMode(agent, input, signal, mode) {
    if (mode === 'in-process') {
      const environment = await detectRunnerEnvironment(this.config, process.env)
      const missing = [
        environment.daemon?.live === true ? undefined : 'a live CensorFS daemon socket',
        environment.commands?.censorfs === undefined ? 'censorfs' : undefined,
        environment.commands?.mounter === undefined ? 'censorfs-mounter' : undefined,
        environment.commands?.node === undefined ? 'node' : undefined,
        environment.commands?.bwrap === undefined ? 'bwrap' : undefined,
        environment.fuse === true ? undefined : '/dev/fuse',
      ].filter(Boolean)
      if (missing.length > 0) {
        throw new Error(`branch_explore_inprocess prerequisites unavailable: ${missing.join(', ')}; run /censorfs-doctor for details`)
      }
      this.config.runnerIsolation.cgroupEnabled = environment.cgroupEnabled === true
      this.config.runnerIsolation.environment = environment
    }
    if (mode === 'external') {
      if (this.config.inProcessOnly) {
        throw new Error('branch_explore is unavailable: this deployment is in-process only (inProcessOnly=true); use branch_explore_inprocess')
      }
      if (this.config.childCommand === undefined) {
        throw new Error('branch_explore requires config.childCommand (the external worker command); set DSH_CENSORFS_CHILD_COMMAND or use branch_explore_inprocess')
      }
    }
    const strategies = validateStrategies(input.strategies)
    if (mode === 'in-process' && this.config.maxTokens !== undefined && this.config.maxTokens < strategies.length) {
      throw new Error(`maxTokens must be at least the number of strategies (${strategies.length}) for in-process exploration`)
    }
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
    const expectedHead = await this.cli.head(this.config.branch)
    const runId = `explore-${randomUUID()}`
    const started = Date.now()
    append(agent.session, 'exploration-started', {
      runId,
      task: input.task,
      branch: this.config.branch,
      fsEnabled: true,
      mode,
      expectedHead,
      validationProfile,
      previewProfile,
      strategies,
      isolation: this.isolationSnapshot(),
      startedAt: started,
    })

    const perVariantMaxTokens = this.config.maxTokens === undefined
      ? undefined
      : Math.max(1, Math.floor(this.config.maxTokens / strategies.length))
    const settled = await Promise.all(strategies.map((strategy) =>
      this.runVariant(agent, runId, input.task, strategy, expectedHead, checks, validationProfile, signal, mode, perVariantMaxTokens)))
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
    return losslessJson({
      runId,
      expectedHead,
      ranking,
      variants: settled.map((variant) => ({
        variantId: variant.variantId,
        label: variant.label,
        candidateId: variant.candidate?.candidate_id,
        generationId: variant.generation?.generation_id,
        requiredPassed: variant.validation?.requiredPassed ?? null,
        validation: variant.validation?.checks?.map((check) => ({
          name: check.name,
          required: check.required,
          passed: check.passed,
          durationMs: check.durationMs,
        })),
        changedPaths: variant.pathDiff?.map((diff) => diff.path) ?? [],
        summary: variant.result?.summary,
        error: variant.error,
      })),
    })
  }

async runVariant(agent, runId, task, strategy, expectedHead, checks, validationProfile, signal, mode, maxTokens) {
    const started = Date.now()
    let opened
    let prepared
    let tmpDir
    let worker
    let childSessionId
    try {
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
      append(agent.session, 'variant-running', {
        runId,
        variantId: strategy.id,
        label: strategy.label,
        strategy: strategy.instruction,
        ticketId: opened.ticket.ticket_id,
        txId: opened.tx.tx_id,
        viewId: opened.view.view_id,
        startedAt: started,
      })

      const workerPrompt = [{
        type: 'text',
        text: [
          `Task: ${task}`,
          `Your distinct strategy: ${strategy.label} - ${strategy.instruction}`,
          'Work directly in /workspace. Inspect the project, modify real files, and run useful checks.',
          'Keep your changes scoped to this strategy. Finish with a concise evidence-based summary.',
          'Do not start development servers, preview servers, watch processes, GUI loops, or other commands that wait indefinitely. Use only finite checks during implementation. Preview is started separately from the frozen Candidate.',
          `Temporary/build/cache output must go under ${tmpDir}; do not write Harness sessions, credentials, or model logs into /workspace.`,
        ].join('\n\n'),
      }]
      worker = await this.runWorker({
        mode,
        agent,
        runId,
        strategy,
        opened,
        tmpDir,
        prompt: workerPrompt,
        signal,
        maxTokens,
      })
      childSessionId = worker.sessionId
      if (worker.stopReason !== 'completed') {
        throw new Error(`worker stopped with ${worker.stopReason}: ${outputText(worker.output)}`)
      }
const summary = outputText(worker.output)
      prepared = await this.cli.prepare({
        ticketId: opened.ticket.ticket_id,
        viewId: opened.view.view_id,
        runId,
        variantId: strategy.id,
      })
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
          mounterCommand: this.config.mounterCommand,
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
        pathDiff: prepared.path_diff,
        textDiff: prepared.text_diff,
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
          pathDiff: prepared.path_diff,
          textDiff: prepared.text_diff,
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

async runWorker({ mode, agent, runId, strategy, opened, tmpDir, prompt, signal, maxTokens }) {
    const binding = {
      runId,
      variantId: strategy.id,
      viewId: opened.view.view_id,
      uid: opened.view.owner_uid,
      gid: opened.view.owner_gid,
      tmpDir,
    }
    if (mode === 'external') {
      const run = await this.provider.withBinding(binding, () => this.ctx.subagents.start(this.provider.name, {
        label: strategy.label,
        prompt,
        parent: agent,
        signal,
      }))
      try {
        return { sessionId: run.id, provider: this.provider.name, mode, ...(await run.result) }
      } finally {
        try {
          await run.dispose()
        } catch (error) {
          this.ctx.logger?.warn?.(`worker dispose failed: ${errorText(error)}`)
        }
      }
    }

    const isolation = this.config.runnerIsolation
    // Capture per-attempt facts before launching: the shared cgroupEnabled
    // flag can be flipped by a concurrent variant's fallback, so the retry
    // decision must be based on what THIS attempt actually did, and the
    // reported fromLevel must be the level this attempt started from.
    const attemptedCgroup = mode === 'in-process' && isolation.cgroupEnabled === true
    const originalFromLevel = isolation.environment?.effectiveLevel ?? 'lifecycle'
    let runner
    try {
      runner = await this.runnerManager.launch(binding, signal)
    } catch (error) {
      // auto runtime fallback: a cgroup-backed launch/readiness failure may
      // mean the delegated subtree became unusable after the startup probe
      // accepted it. Clean up (launch already disposed the failed record),
      // downgrade to process isolation once, and retry. required fails closed
      // without retry. The downgrade flips the shared cgroupEnabled flag, so
      // the retry itself can never re-enter this branch (no infinite retry).
      // Every parallel variant that actually attempted cgroup retries once.
      if (mode === 'in-process' && !signal?.aborted
        && isolation.mode === 'auto' && attemptedCgroup) {
        const fallback = {
          runId,
          variantId: strategy.id,
          fromLevel: originalFromLevel,
          originalFromLevel,
          toLevel: 'process',
          warning: `cgroup-backed Runner launch failed; auto fallback to process isolation for ${strategy.id}: ${errorText(error)}`,
          at: Date.now(),
        }
        isolation.cgroupEnabled = false
        const { cgroupRoot, ...rest } = isolation.environment ?? {}
        isolation.environment = {
          ...rest,
          effectiveLevel: 'process',
          cgroupEnabled: false,
          warnings: [...(rest.warnings ?? []), fallback.warning],
        }
        append(agent.session, 'isolation-fallback', fallback)
        this.ctx.logger?.warn?.(`censorfs isolation fallback: ${fallback.warning}`)
        try {
          runner = await this.runnerManager.launch(binding, signal)
        } catch (retryError) {
          // The retry's own cleanup already ran inside launch; surface the
          // original failure so the variant records the true first cause.
          throw error
        }
      } else {
        throw error
      }
    }
    let run
    try {
      run = await this.inProcessProvider.withBinding({ ...binding, runner }, () => this.ctx.subagents.start(
        this.inProcessProvider.name,
        {
          label: strategy.label,
          prompt,
          parent: agent,
          signal,
          maxDepth: this.config.inProcessMaxDepth,
           ...(maxTokens === undefined ? {} : { maxTokens }),
        },
      ))
this.registerVariantBinding(run.id, agent, runId, strategy.id)
      return { sessionId: run.id, provider: this.inProcessProvider.name, mode, ...(await run.result) }
    } finally {
      if (run !== undefined) {
        try {
          await run.dispose()
        } catch (error) {
          this.ctx.logger?.warn?.(`worker dispose failed: ${errorText(error)}`)
        }
        this.unregisterVariantBinding(run.id)
      } else {
        try {
          await this.runnerManager.disposeRecord(runner)
        } catch (error) {
          this.ctx.logger?.warn?.(`runner dispose failed: ${errorText(error)}`)
        }
      }
    }
  }

  trackSubagentStart(info) {
    if (info.provider !== this.inProcessProvider.name) return
    this.trackedSubagents.set(info.id, { sessionId: info.id, startedAt: Date.now(), activities: [] })
  }

  trackSubagentEnd(info) {
    const tracked = this.trackedSubagents.get(info.id)
    if (tracked !== undefined) {
      tracked.stopReason = info.stopReason
      tracked.endedAt = Date.now()
    }
  }

  trackSubagentActivity(session, event) {
    const tracked = this.trackedSubagents.get(session.id)
    if (tracked === undefined || !ACTIVITY_EVENT_TYPES.has(event.type)) return
    const activity = { ...summarizeActivityEvent(event), at: Date.now() }
    tracked.activities.push(activity)
    const binding = this.variantBindings.get(session.id)
    if (binding !== undefined) {
      append(binding.agent.session, 'variant-activity', {
        runId: binding.runId,
        variantId: binding.variantId,
        activity,
      })
    }
  }

  registerVariantBinding(childSessionId, agent, runId, variantId) {
    this.variantBindings.set(childSessionId, { agent, runId, variantId })
    // 树图分组信号：全局 subagent/start 钩子落 subagent-started 时本 binding 尚未
    // 注册（sessionId 是 ctx.subagents.start 返回后才知道的），节点带的是 harness
    // 分配的 subagent runId 而非 explore runId → 每个 worker 各自一组，判不出
    // 同属一个 explore。这里补写 variant/binding 活动，客户端折叠时用它覆盖
    // 节点上的 runId/variantId（sgApplyEvent），节点归入对应 explore 分组。
    append(agent.session, 'subagent-activity', {
      sessionId: childSessionId,
      rootSessionId: agent.session.id,
      activity: {
        type: 'variant/binding',
        runId,
        variantId,
        preview: `explore variant ${variantId}`,
        at: Date.now(),
      },
    })
  }

  unregisterVariantBinding(childSessionId) {
    this.variantBindings.delete(childSessionId)
  }

  state(agent, runId) {
    // 重启残留 reconcile（幂等）：上一进程启动的 run，其 worker 已随进程死亡，
    // 但 terminal 事件没落盘 —— 卡片/树图会永远定格 running。这里在读取时补写。
    const reconciledNodes = reconcileInterruptedSubagents(agent.session)
    // rc.2 的 Session 用 snapshotEvents() 取事件快照（不再有 events 数组属性）。
    let state = foldExploration(agent.session.snapshotEvents(), runId)
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
      if (reconciledVariants > 0) state = foldExploration(agent.session.snapshotEvents(), runId)
    }
    // 补写的事件要落盘（fire-and-forget），否则下次重启后磁盘上仍是旧状态。
    if (reconciledNodes + reconciledVariants > 0) this.ctx.sessions.flush(agent.session).catch(() => {})
    return state
  }

  // 对已 prepared 的 Candidate 重新验证（显式 profile 或 default）。
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
      mounterCommand: this.config.mounterCommand,
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

  // The isolation snapshot recorded in exploration-started comes from the
  // startup probe; the same object is written to the log so the exploration
  // Session and the harness log agree on what isolation actually ran.
  isolationSnapshot() {
    const env = this.config.runnerIsolation.environment
    if (env === undefined) return undefined
    const base = { requestedMode: env.requestedMode, minimumLevel: env.minimumLevel }
    if (env.error !== undefined) return { ...base, error: env.error }
    return {
      ...base,
      effectiveLevel: env.effectiveLevel,
      cgroupEnabled: env.cgroupEnabled,
      controllers: env.controllers ?? [],
      ...(env.cgroupRoot === undefined ? {} : { cgroupRoot: env.cgroupRoot }),
      warnings: env.warnings ?? [],
    }
  }

  // Fresh read-only probe for /censorfs-doctor. Never throws: a fail-closed
  // isolation policy is reported in the `error` field alongside every detail.
  async doctor() {
    const report = await probeRunnerEnvironment(this.config, process.env)
    return { ...report, legacyRunnerCgroup: this.config.runnerCgroup.enabled === true }
  }

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
        if (loser.variantId === variantId || loser.ticketId === undefined) continue
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
    // 先真正停止 worker，再释放 daemon ticket。只 abort ticket 会让 worker
    // 继续跑：卡片 aborted 而代理图一直 RUNNING。external（FUSE）走
    // cancel → 3s SIGTERM 升级；in-process 走协作式 child.cancel。
    // worker 死后现有 finally / subagent-end 钩子会落 subagent-ended，树图随之收敛。
    const provider = state.mode === 'in-process' ? this.inProcessProvider : this.provider
    provider.cancel(state.runId, variant.variantId)
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
    const child = spawn(this.config.mounterCommand, [
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
