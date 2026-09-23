/**
 * Host-side interaction bridge (CONTROL PLANE ONLY).
 *
 * Approval and user-question requests originate inside the resident WORKER (the
 * process that owns the native agent-loop and the tool call), but the only
 * interactive answerer in dsh is the browser: it listens on the Host's remote
 * waterfall (`ctx.remote.$on('approval/request')` /
 * `('user-questions/request')`, allowlisted with mode `waterfall` by
 * `@deepseek-ai/dsh-api-remotes`). The worker has neither that bridge nor a
 * connected client, so its ask falls through to the documented fail-closed
 * outcome (`unavailable` / `NO_PROVIDER`).
 *
 * This bridge closes the loop worker → host → browser → host → worker:
 *   1. the worker forwards the ask over stdout (`ACASK`),
 *   2. the host dispatches the SAME waterfall the native web surface uses, with
 *      the host agent as the scoped subject so the browser routes the prompt to
 *      the right session (the client resolves the session from the dispatch
 *      scope — `ctx.sessions.scopeOf(owner)` — not from the payload),
 *   3. the human's answer travels back over the worker's stdin (`answer` frame).
 *
 * ── What this module must never do ──
 * It writes NO session data. It holds no Session and never calls
 * `session.append`; the audit pair (`approval/asked` + `approval/decided`), the
 * `tool/result`, and every other durable fact stay authored by the worker — they
 * reach the UI exactly like the rest of the worker's stream, through the driver's
 * 1:1 mirror ("worker is the content authority; the host never synthesizes
 * session data"). Dispatching a waterfall is an interaction request, not session
 * content, and it is deliberately the raw waterfall rather than the host's own
 * `ctx.approval.request()` / `ctx.userQuestions.ask()`: those services would
 * append a second audit pair into the same session, and the approval service
 * additionally requires an open turn on its own agent's session.
 *
 * The dispatched request carries an AbortSignal. The gateway ties the pending
 * remote event to that signal (`projected.signal` → cancel frame), so aborting
 * it dismisses the browser prompt — which is how a withdrawal (stop button,
 * aborted tool call, dead worker) is propagated. With no client attached the
 * gateway settles the dispatch with `next()`, which this bridge reports to the
 * worker as "no answer", so the worker fails closed exactly as it does today.
 */
import { randomUUID } from 'node:crypto'
import { scopeTarget } from '@deepseek-ai/dsh-scope'

/** Approval outcomes the native service accepts; anything else fails closed. */
const APPROVAL_OUTCOMES = new Set(['allowed-once', 'rejected', 'cancelled', 'unavailable'])

/**
 * Optional cap on how long one forwarded ask may stay unanswered before the
 * worker is told "no answer" and fails closed. 0 (the default) waits like stock
 * dsh, where a pending prompt lives until the human answers or the dispatch is
 * cancelled.
 */
function askTimeoutMs() {
  const raw = Number(process.env.AGENTCENSOR_ASK_TIMEOUT_MS ?? '0')
  return Number.isFinite(raw) && raw > 0 ? raw : 0
}

/**
 * Create the host-side bridge.
 * @param ctx - host root context owning the remote waterfall (the plugin's ctx).
 * @returns bridge with `ask`, `withdraw`, and `disposeAgent`.
 */
export function createInteractionBridge(ctx) {
  /**
   * One dispatch per forwarded ask.
   * @type {Map<string, {agent: object, id: string, kind: string, write: (frame: object) => void, controller: AbortController, timer: NodeJS.Timeout | undefined, settled: boolean}>}
   */
  const pending = new Map()

  const keyOf = (agent, id) => `${String(agent?.id ?? '?')}:${String(id)}`

  /** Stop owning one dispatch: clear its timer and abort its dispatched signal. */
  const release = (entry) => {
    if (entry.settled) return false
    entry.settled = true
    if (entry.timer !== undefined) clearTimeout(entry.timer)
    pending.delete(keyOf(entry.agent, entry.id))
    // Aborting the signal the dispatch was bound to makes the gateway cancel the
    // pending remote event, so the browser prompt is dismissed.
    entry.controller.abort()
    return true
  }

  /** Write one answer frame back to the worker, swallowing a closed stdin. */
  const reply = (entry, payload) => {
    try {
      entry.write({ type: 'answer', kind: entry.kind, id: entry.id, ...payload })
    } catch (error) {
      process.stdout.write(`[agentcensor] interaction answer write failed: ${String(error)}\n`)
    }
  }

  /**
   * Dispatch one waterfall with the host agent as the scoped subject, the same
   * dispatch the native services perform.
   */
  const dispatch = async (event, request, fallback) => {
    return await ctx.waterfall(scopeTarget(request.agent, request.agent), event, request, fallback)
  }

  /**
   * Forward one worker ask to the browser and write the answer back.
   * @param agent - host driver agent of that session (routing subject).
   * @param ask - `{id, kind:'approval'|'question', …payload}` from the worker.
   * @param write - frame writer for that worker's stdin.
   * @returns resolution once the answer frame has been written.
   */
  async function ask(agent, ask, write) {
    const id = typeof ask?.id === 'string' && ask.id.length > 0 ? ask.id : randomUUID()
    const kind = ask?.kind === 'approval' || ask?.kind === 'question' ? ask.kind : 'unsupported'
    const entry = {
      agent,
      id,
      kind,
      write,
      controller: new AbortController(),
      timer: undefined,
      settled: false,
    }
    if (pending.has(keyOf(agent, id))) {
      reply({ ...entry, kind: ask?.kind }, { ok: false, message: 'duplicate ask id' })
      return
    }
    const timeoutMs = askTimeoutMs()
    if (timeoutMs > 0) {
      entry.timer = setTimeout(() => {
        process.stdout.write(
          `[agentcensor] interaction ask id=${id} timed out after ${timeoutMs}ms; failing closed\n`,
        )
        if (release(entry)) reply(entry, { ok: false, message: 'timed out waiting for the browser' })
      }, timeoutMs)
      entry.timer.unref?.()
    }
    pending.set(keyOf(agent, id), entry)
    process.stdout.write(
      `[agentcensor] interaction ask kind=${kind} id=${id} session=${String(agent?.id)} → browser\n`,
    )
    try {
      if (kind === 'approval') {
        const outcome = await dispatch('approval/request', {
          agent,
          ...(typeof ask.toolName === 'string' ? { toolName: ask.toolName } : {}),
          ...(typeof ask.callId === 'string' ? { callId: ask.callId } : {}),
          ...(typeof ask.reason === 'string' ? { reason: ask.reason } : {}),
          signal: entry.controller.signal,
        }, () => Promise.resolve('unavailable'))
        const normalized = APPROVAL_OUTCOMES.has(outcome) ? outcome : 'unavailable'
        if (release(entry)) {
          reply(entry, { ok: true, outcome: normalized })
          process.stdout.write(
            `[agentcensor] interaction answer kind=approval id=${id} outcome=${normalized}\n`,
          )
        }
        return
      }
      if (kind === 'question') {
        const questions = Array.isArray(ask.questions) ? ask.questions : []
        const value = await dispatch('user-questions/request', {
          questions,
          agent,
          signal: entry.controller.signal,
        }, () => Promise.reject(new Error('no user-questions answerer accepted the request')))
        const answers = Array.isArray(value?.answers) ? value.answers : undefined
        if (release(entry)) {
          if (answers === undefined) reply(entry, { ok: false, message: 'malformed answer' })
          else {
            reply(entry, { ok: true, answers })
            process.stdout.write(
              `[agentcensor] interaction answer kind=question id=${id} answers=${answers.length}\n`,
            )
          }
        }
        return
      }
      if (release(entry)) reply(entry, { ok: false, message: 'unsupported ask kind' })
    } catch (error) {
      // No answerer, no client, a throwing answerer, or a withdrawn dispatch:
      // hand the worker back to its own fail-closed path.
      if (release(entry)) {
        reply(entry, { ok: false, message: error?.message ?? String(error) })
        process.stdout.write(
          `[agentcensor] interaction ask id=${id} unanswered (${error?.message ?? String(error)}); worker fails closed\n`,
        )
      }
    }
  }

  /**
   * Withdraw one pending ask (stop button, aborted call, worker shutdown or
   * timeout). Aborting the dispatched signal dismisses the browser prompt.
   * @returns whether an ask was pending.
   */
  function withdraw(agent, id, reason = 'withdrawn') {
    const entry = pending.get(keyOf(agent, id))
    if (entry === undefined) return false
    process.stdout.write(`[agentcensor] interaction withdraw id=${String(id)} (${reason})\n`)
    if (release(entry)) reply(entry, { ok: false, message: reason })
    return true
  }

  /** Withdraw every pending ask of one agent (stop, worker exit, dispose). */
  function disposeAgent(agent, reason = 'cancelled') {
    let count = 0
    for (const entry of [...pending.values()]) {
      if (entry.agent !== agent) continue
      if (release(entry)) {
        reply(entry, { ok: false, message: reason })
        count += 1
      }
    }
    if (count > 0) {
      process.stdout.write(
        `[agentcensor] interaction withdraw session=${String(agent?.id)} pending=${count} (${reason})\n`,
      )
    }
    return count
  }

  return { ask, withdraw, disposeAgent }
}
