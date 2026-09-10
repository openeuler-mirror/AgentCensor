import { AsyncLocalStorage } from 'node:async_hooks'
import { randomUUID } from 'node:crypto'
import { foldConsumedWork } from '@deepseek-ai/dsh-agent'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { SessionId } from '@deepseek-ai/dsh-session'
import {
  appendDelegatedPolicyOverrides,
  applyChildComposition,
  assertSubagentMaxDepth,
  captureDelegatedPolicyOverrides,
  childSessionMeta,
  finalAssistantOutput,
  resolveChildAgentOptions,
  resolveChildDepth,
} from '@deepseek-ai/dsh-subagent'

function stopReason(reason) {
  if (reason?.kind === 'completed') return 'completed'
  if (reason?.kind === 'max-tokens') return 'max-tokens'
  if (reason?.kind === 'aborted') return 'aborted'
  if (reason?.kind === 'blocked') return 'refusal'
  return 'error'
}

function attachDescriptor(childCtx, descriptor) {
  let appended = false
  childCtx.on('agent/pre-step', async ({ agent }, next) => {
    const decision = await next()
    if (!appended && decision.kind === 'enter') {
      appended = true
      agent.session.append('subagent/descriptor', descriptor)
    }
    return decision
  })
}

export class InProcessCensorFsProvider {
  capabilities = { outputSchema: false, depthLimit: true, toolFilter: true, persona: true }
  inheritsParentContext = false

  constructor(name, runnerManager) {
    this.name = name
    this.runnerManager = runnerManager
    this.bindings = new AsyncLocalStorage()
    // runId:variantId -> 取消函数，供 runtime.abortVariant 真正停止 worker
    // （对称于 FuseProvider.cancelChannels；只 abort ticket 不停 worker 会让
    // 代理图一直 RUNNING）。
    this.activeCancels = new Map()
  }

  /**
   * 协作式取消某个运行中的 in-process worker（child.cancel）；
   * 若 worker 卡在不可中断的工具调用里，5s 后强制 dispose 兜底
   * （handle.dispose 销毁 agent ctx，中断进行中的步骤）。
   * @returns true 已触发；false 该 variant 无活动 worker（未运行或已结束）
   */
  cancel(runId, variantId) {
    const key = `${runId}:${variantId}`
    const entry = this.activeCancels.get(key)
    if (entry === undefined) return false
    entry.cancel()
    entry.timer = setTimeout(() => {
      // entry 仍在 Map 里 = worker 没 settle（dispose 会删 entry 并清 timer）
      if (this.activeCancels.get(key) === entry) entry.force()
    }, 5000)
    entry.timer.unref?.()
    return true
  }

  withBinding(binding, action) {
    return this.bindings.run(binding, action)
  }

  async start(request) {
    assertSubagentMaxDepth(request.maxDepth)
    if (request.signal.aborted) throw new Error('CensorFS in-process worker was cancelled before startup')
    const binding = this.bindings.getStore()
    if (binding === undefined) throw new Error('censorfs-inprocess is private to branch_explore_inprocess')

    const parent = request.parent
    const childDepth = resolveChildDepth(parent, request.maxDepth)
    const childId = SessionId(randomUUID())
    const inherited = captureDelegatedPolicyOverrides(parent)
    const manager = this.runnerManager
    let boundAgent

    const handle = await parent.ctx.agents.create({
      sessionId: childId,
      meta: childSessionMeta(parent, childDepth, 0),
      agentOptions: resolveChildAgentOptions(parent, request.agentOptions, childDepth),
      signal: request.signal,
      setup(childCtx) {
        appendDelegatedPolicyOverrides(childCtx.agent.session, inherited)
        applyChildComposition(childCtx, parent, {
          persona: request.persona,
          toolFilter: request.toolFilter,
        })
        manager.bind(childCtx.agent, binding.runner)
        boundAgent = childCtx.agent
        childCtx.effect(() => () => manager.disposeAgent(childCtx.agent), 'censorfs-runner.agent-dispose')
        attachDescriptor(childCtx, request.descriptor)
        return { commit: () => manager.commitBinding(childCtx.agent, binding.runner) }
      },
    })

    const child = handle.agent
    let cancelled = false
    const onAbort = () => {
      cancelled = true
      child.cancel({ kind: 'parent' })
    }
    request.signal.addEventListener('abort', onAbort, { once: true })
    if (request.signal.aborted) onAbort()

    // 经闭包捕获（不依赖 this）：供 provider.cancel() 的兜底 timer 与
    // 返回给 runtime 的 handle 安全共享。
    const activeCancels = this.activeCancels
    const cancelKey = `${binding.runId}:${binding.variantId}`

    const result = (async () => {
      try {
        if (!cancelled) {
          child.followup(createUserMessage({ content: request.prompt, source: { kind: 'user' } }))
          await child.whenIdle()
        }
        const end = foldConsumedWork(child.session.events).end
        const recorded = stopReason(end?.data.reason)
        return {
          output: finalAssistantOutput(child.session.events) ?? [],
          stopReason: cancelled && recorded !== 'completed' ? 'aborted' : recorded,
        }
      } finally {
        request.signal.removeEventListener('abort', onAbort)
      }
    })()

    // 幂等：cancel() 的 5s 强制兜底与 runWorker 的 finally 都可能触发 dispose。
    let disposed = false
    const dispose = async () => {
      if (disposed) return
      disposed = true
      request.signal.removeEventListener('abort', onAbort)
      const entry = activeCancels.get(cancelKey)
      if (entry?.timer !== undefined) clearTimeout(entry.timer)
      activeCancels.delete(cancelKey)
      cancelled = true
      const settlements = await Promise.allSettled([handle.dispose(), result])
      if (boundAgent !== undefined) await manager.disposeAgent(boundAgent)
      if (settlements[0].status === 'rejected') throw settlements[0].reason
    }
    activeCancels.set(cancelKey, {
      cancel: onAbort,
      force: () => { dispose().catch(() => {}) },
    })

    return {
      id: childId,
      localAgent: child,
      result,
      dispose,
    }
  }
}
