/**
 * Session worker runner (external plugin file, no dsh source changes).
 *
 * Runs INSIDE a per-session dsh process (`dsh --profile headless` + overlay)
 * spawned lazily by the host WebDriverAgent. The worker is a FULL dsh agent
 * process for ONE session: it boots once (create for a brand-new session,
 * resume for an existing one) through the NATIVE agent-loop (agent-loop is NOT
 * disabled in the worker profile), then serves every subsequent user message
 * of that session as a followup on the SAME live agent. The process is NOT
 * destroyed when a turn ends — it stays alive until the host asks it to shut
 * down (or stdin closes because the host died).
 *
 * The worker's persistence root is PRIVATE (the host seeds a snapshot copy of
 * the durable log into a per-worker private persistence root), so the worker
 * never writes the shared JSONL — the HOST is the single durable writer of the
 * shared log (why: the persistence coordinator auto-persists every live append
 * and the JSONL backend has no cross-process lock; see the host plugin header).
 * The host replays the worker's event stream transparently.
 *
 * ── Stdout protocol (worker → host) ──
 *   ACEVT\t<json>    one appended session event {type,seq,data,surfaceOp?,sourceEventSeqs?}
 *   ACREADY\t<json>  booted & ready: {mode,firstSeq,model:{provider,model}}
 *   ACIDLE\t<json>   one turn finished (agent idle, session flushed): {seq}
 *   ACERR\t<json>    runner-level turn failure (worker stays alive): {message}
 *   ACCANCEL\t<json> cancel handed to the native agent: {cause,keepInbox,seq,applied}
 *   ACASK\t<json>    approval/question ask forwarded to the host: {id,kind,...payload}
 *   ACWITHDRAW\t<json> withdrawal of one forwarded ask: {id}
 * Everything else on stdout/stderr is host log passthrough.
 *
 * ── Stdin protocol (host → worker, one JSON per line) ──
 *   {"type":"message","message":{content,source,id?,role?}}   original user message
 *   {"type":"cancel","cause":{kind},"keepInbox":bool}         native turn cancellation
 *   {"type":"answer","kind":"approval","id":…,"ok":true,"outcome":"allowed-once"}
 *   {"type":"answer","kind":"question","id":…,"ok":true,"answers":[…]}
 *   {"type":"answer","…","ok":false}                          no answer available
 *   {"type":"shutdown"}                                       graceful exit
 * stdin EOF ⇒ graceful exit (host process is gone).
 *
 * ── Human interaction (approval + ask_user_question) ──
 * The only interactive answerer in dsh is the browser, and it listens on the
 * HOST's remote waterfall — this worker has no such bridge and no client, so a
 * native ask would fail closed immediately (the approval service returns
 * 'unavailable' and the question seam rejects NO_PROVIDER). This worker therefore
 * registers its own answerers that forward the ask to the host (`ACASK`) and wait
 * for the host's answer frame; the host dispatches the very same waterfall to the
 * browser. When the host has nothing to offer (no browser, no answerer, worker
 * shutting down) the answerers delegate with `next()`, so the native fail-closed
 * outcome stays exactly as it is without the plugin. A withdrawn ask (stop
 * button, aborted tool call) returns the native 'cancelled'/aborted outcome and
 * tells the host to dismiss the prompt.
 *
 * Cancellation is native and in-process: the host's stop button (and any other
 * caller of `agent.cancel`) is forwarded here, and this worker aborts its own
 * live turn through `agent.cancel(cause, {keepInbox})`. The resulting events
 * (`tool/result` closers for started calls, `step/end`,
 * `turn/end {reason:{kind:'aborted',reason}}`) are ordinary session events, so
 * they mirror back to the host exactly like any other turn output and the
 * durable tail is closed — no process kill, no crash repair on the next resume,
 * and the resident agent stays usable for the next message.
 *
 * Env contract (set by the host driver — task-channel plumbing only, no
 * credential/model/configuration values):
 *   AGENTCENSOR_SESSION_ID   durable session identity to create or resume
 *   AGENTCENSOR_MODE         'create' (fresh) | 'resume' (existing durable log)
 * The worker runs with DSH_HOME = the HOST's home (config/credentials are read
 * in place, like stock dsh); only its writable data roots are private.
 */
import { randomUUID } from 'node:crypto'
import { createInterface } from 'node:readline'
import { brandString } from '@deepseek-ai/dsh-brand'
import { installModelSelection } from '@deepseek-ai/dsh-agent'

export const name = 'agentcensor-worker'
export const inject = ['agentDefaultModel', 'agents', 'sessions']

export function apply(ctx) {
  const exit = ctx.get('appExit')
  void run(ctx).then((code) => {
    if (exit === undefined) process.exit(code)
    else exit(code)
  }).catch((error) => {
    process.stderr.write(`agentcensor-worker failed: ${error?.stack ?? String(error)}\n`)
    if (exit === undefined) process.exit(1)
    else exit(1)
  })
}

/**
 * Newest per-session model choice in the replayed durable log: scanning from
 * the tail, the first `model/selection` (UI picker change) or `request/header`
 * (a request that actually ran) wins — the same rule the host driver uses.
 */
function durableSelection(session) {
  const events = session.snapshotEvents()
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index]
    if (event.type === 'model/selection') {
      const data = event.data ?? {}
      if (typeof data.provider === 'string' && typeof data.model === 'string') {
        return {
          provider: data.provider,
          model: data.model,
          ...(typeof data.reasoningEffort === 'string' ? { reasoningEffort: data.reasoningEffort } : {}),
        }
      }
    }
    if (event.type === 'request/header') {
      const config = event.data?.header?.config
      if (typeof config?.provider === 'string' && typeof config.model === 'string') {
        return {
          provider: config.provider,
          model: config.model,
          ...(typeof config.reasoningEffort === 'string' ? { reasoningEffort: config.reasoningEffort } : {}),
        }
      }
    }
  }
  return undefined
}

async function run(ctx) {
  const agents = ctx.get('agents')
  const defaultModel = ctx.get('agentDefaultModel')
  const sessions = ctx.get('sessions')
  const loader = ctx.get('loader')
  if (agents === undefined || defaultModel === undefined || sessions === undefined) {
    process.stderr.write('agentcensor-worker: required services unavailable\n')
    return 1
  }
  if (loader !== undefined) await loader.await()

  const sessionId = process.env.AGENTCENSOR_SESSION_ID
  const mode = process.env.AGENTCENSOR_MODE === 'resume' ? 'resume' : 'create'
  if (!sessionId) {
    process.stderr.write('agentcensor-worker: AGENTCENSOR_SESSION_ID is required\n')
    return 1
  }

  // Subscribe BEFORE create/resume so publish-time boot appends (permission/
  // preset, sandbox/mode, approval/policy) are streamed too; the listener
  // stays registered for the whole process life (multi-turn).
  ctx.on('session/event', (session, event) => {
    if (session.id !== sessionId) return
    const { type, seq, data, surfaceOp, sourceEventSeqs } = event
    const line = JSON.stringify({
      type,
      seq,
      data,
      ...(surfaceOp === undefined ? {} : { surfaceOp }),
      ...(sourceEventSeqs === undefined ? {} : { sourceEventSeqs }),
    })
    process.stdout.write(`ACEVT\t${line}\n`)
  }, { global: true })

  // Model selection is not forwarded via env: the worker shares the host
  // DSH_HOME (credentials, settings.yaml, machine-level cordis.patch.yml, env
  // layers), so the agent is constructed with this profile's default selection
  // (host settings `agent-default-model:` section when present)…
  const fallback = defaultModel.currentSelection()
  const agentOptions = { provider: fallback.provider, model: fallback.model }
  const handle = mode === 'resume'
    ? await agents.resume({
        resumeSessionId: brandString(sessionId),
        agentOptions,
      })
    : await agents.create({
        sessionId: brandString(sessionId),
        meta: { cwd: process.cwd() },
        agentOptions,
      })
  const agent = handle.agent
  const firstSeq = agent.session.seq

  // …then the session's current choice is derived from this worker's own
  // durable log replay (the seed the host copied into its private persistence
  // root): the newest `model/selection` or `request/header` config in the
  // conversation wins.
  const selection = durableSelection(agent.session) ?? fallback
  installModelSelection(agent.ctx, { current: selection, assembled: undefined })

  // Register the human-interaction answerers BEFORE any turn can run: a tool
  // call in the first message must already be able to reach the browser.
  const asks = createAskBroker()
  registerInteractionAnswerers(ctx, asks)

  const censorscope = process.env.CENSORSCOPE_SESSION_ID ?? ''
  process.stdout.write(
    `agentcensor-worker resident session=${sessionId} mode=${mode} seq=${firstSeq} model=${selection.provider}/${selection.model}${censorscope.length > 0 ? ` censorscopeSession=${censorscope}` : ''}\n`,
  )
  process.stdout.write(`ACREADY\t${JSON.stringify({
    mode,
    firstSeq,
    model: { provider: selection.provider, model: selection.model },
  })}\n`)

  return controlLoop(agent, sessions, asks)
}

/** Serve messages, cancellations, and interaction answers until shutdown or EOF. */
async function controlLoop(agent, sessions, asks) {
  let chain = Promise.resolve()
  // Messages accepted but not yet finished: a cancel arriving while the agent is
  // still idle belongs to the turn one of these is about to open.
  let queuedMessages = 0
  // A cancel that arrives before its message reaches the agent's driver; held
  // only for the turn that message opens (a cancel must never arm later work).
  const cancelLatch = { current: undefined }
  const ackCancel = (cancel, applied) => {
    process.stdout.write(`ACCANCEL\t${JSON.stringify({
      cause: cancel.cause,
      keepInbox: cancel.keepInbox,
      seq: agent.session.seq,
      applied,
    })}\n`)
  }
  const rl = createInterface({ input: process.stdin })
  for await (const raw of rl) {
    if (raw.trim() === '') continue
    let frame
    try {
      frame = JSON.parse(raw)
    } catch (error) {
      process.stderr.write(`agentcensor-worker: unparsable stdin frame: ${String(error)}\n`)
      continue
    }
    if (frame?.type === 'message') {
      const message = frame.message
      if (!isUserMessage(message)) {
        process.stdout.write(`ACERR\t${JSON.stringify({ message: 'invalid message frame' })}\n`)
        continue
      }
      queuedMessages += 1
      chain = chain
        .then(() => runOneTurn(agent, sessions, message, cancelLatch))
        .catch((error) => {
          // Runner-level turn failure: report and stay alive for the next
          // message. Model-level failures are handled natively inside the
          // agent loop and surface as ordinary session events (replayed).
          process.stdout.write(`ACERR\t${JSON.stringify({ message: error?.message ?? String(error) })}\n`)
        })
        .finally(() => {
          queuedMessages -= 1
        })
    } else if (frame?.type === 'cancel') {
      // Native cancellation, applied outside the message chain so a busy turn is
      // aborted the moment the frame is read. `agent.cancel` is a no-op with no
      // active activity, so a cancel that races the message it belongs to is
      // latched for that message's turn instead of being lost.
      const cancel = {
        cause: cancelCause(frame.cause),
        keepInbox: frame.keepInbox === true,
      }
      if (agent.status === 'running') {
        agent.cancel(cancel.cause, { keepInbox: cancel.keepInbox })
        ackCancel(cancel, 'now')
      } else if (queuedMessages > 0) {
        cancelLatch.current = cancel
        ackCancel(cancel, 'latched')
      } else {
        ackCancel(cancel, 'noop')
      }
    } else if (frame?.type === 'answer') {
      // Late or duplicate answers are discarded: the asker already settled.
      asks.settle(frame)
    } else if (frame?.type === 'shutdown') {
      break
    } else {
      process.stderr.write(`agentcensor-worker: unknown stdin frame type=${String(frame?.type)}\n`)
    }
  }
  await chain.catch(() => undefined)
  // The host is going away: every still-pending ask fails closed now instead of
  // holding a turn open.
  asks.close()
  await sessions.flush(agent.session)
  return 0
}

/**
 * Execute one user message, then report idle (agent finished the turn). A cancel
 * latched before this message reached the agent is applied immediately after the
 * followup so the turn it starts is aborted, never left running; a cancel that
 * loses the race to this turn's own end stays a no-op instead of arming the next
 * turn (native semantics: no active activity, no cancellation).
 */
async function runOneTurn(agent, sessions, message, cancelLatch) {
  agent.followup(message)
  const latched = cancelLatch.current
  if (latched !== undefined) {
    cancelLatch.current = undefined
    agent.cancel(latched.cause, { keepInbox: latched.keepInbox })
    process.stdout.write(`ACCANCEL\t${JSON.stringify({
      cause: latched.cause,
      keepInbox: latched.keepInbox,
      seq: agent.session.seq,
      applied: 'with-message',
    })}\n`)
  }
  await agent.whenIdle()
  cancelLatch.current = undefined
  await sessions.flush(agent.session)
  process.stdout.write(`ACIDLE\t${JSON.stringify({ seq: agent.session.seq })}\n`)
}

/** Normalize a stdin cancel cause to the closed set the native agent accepts. */
function cancelCause(cause) {
  if (cause !== null && typeof cause === 'object') {
    if (cause.kind === 'user' || cause.kind === 'parent') return { kind: cause.kind }
    if (cause.kind === 'hook' && typeof cause.reason === 'string') return { kind: 'hook', reason: cause.reason }
  }
  return { kind: 'user' }
}

/** Whether a value looks like a UserMessage the native followup accepts. */
function isUserMessage(value) {
  return value !== null && typeof value === 'object'
    && Array.isArray(value.content) && value.source !== undefined && typeof value.source === 'object'
}

/**
 * One-forwarded-ask broker: correlates `ACASK` frames with the host's answer
 * frames and settles every ask exactly once (answer, withdrawal, or host gone).
 * Nothing here touches the session log — the answer is handed back to the native
 * approval / user-questions service, which owns the durable audit pair.
 * @returns broker with `ask`, `settle`, `close`.
 */
function createAskBroker() {
  /** id → {finish(result)} for asks this worker is still waiting on. */
  const pending = new Map()
  let closed = false

  const write = (line) => {
    /* EPIPE etc. */
    try { process.stdout.write(line) } catch {}
  }

  return {
    /**
     * Forward one ask to the host and resolve with its answer frame.
     * @param kind - 'approval' or 'question'.
     * @param payload - JSON-safe request fields (agent/signal never travel).
     * @param signal - the calling tool's signal; aborting it withdraws the ask.
     * @returns the host's answer frame, or `{withdrawn:true}` / `{unavailable:true}`.
     */
    ask(kind, payload, signal) {
      if (closed) return Promise.resolve({ unavailable: true })
      const id = randomUUID()
      return new Promise((resolve) => {
        const finish = (result) => {
          if (!pending.delete(id)) return
          /* already gone */
          try { signal?.removeEventListener('abort', onAbort) } catch {}
          resolve(result)
        }
        const onAbort = () => {
          // Tell the host too, so the browser prompt is dismissed rather than
          // left dangling for a turn that is already over.
          write(`ACWITHDRAW\t${JSON.stringify({ id })}\n`)
          finish({ withdrawn: true })
        }
        pending.set(id, { finish })
        if (signal !== undefined) {
          if (signal.aborted) {
            onAbort()
            return
          }
          signal.addEventListener('abort', onAbort, { once: true })
        }
        write(`ACASK\t${JSON.stringify({ id, kind, ...payload })}\n`)
      })
    },

    /** Settle one ask with the host's answer frame; unknown ids are discarded. */
    settle(frame) {
      const id = typeof frame?.id === 'string' ? frame.id : undefined
      const entry = id === undefined ? undefined : pending.get(id)
      if (entry === undefined) return false
      entry.finish(frame)
      return true
    },

    /** Host gone / worker exiting: fail every pending ask closed. */
    close() {
      closed = true
      for (const entry of [...pending.values()]) entry.finish({ unavailable: true })
    },

    /** Number of asks still waiting on the host. */
    get size() {
      return pending.size
    },
  }
}

/**
 * Register the two scoped-waterfall answerers that let a tool call inside this
 * worker reach the human. Each delegates with `next()` whenever the host cannot
 * answer, so the native fail-closed outcome ('unavailable' / NO_PROVIDER) is
 * unchanged when no browser is attached.
 */
function registerInteractionAnswerers(ctx, asks) {
  ctx.on('approval/request', async (request, next) => {
    const answer = await asks.ask('approval', {
      ...(typeof request.toolName === 'string' ? { toolName: request.toolName } : {}),
      ...(typeof request.callId === 'string' ? { callId: request.callId } : {}),
      ...(typeof request.reason === 'string' ? { reason: request.reason } : {}),
    }, request.signal)
    if (answer?.withdrawn === true) return 'cancelled'
    if (answer?.ok === true && typeof answer.outcome === 'string') return answer.outcome
    return next()
  })

  ctx.on('user-questions/request', async (request, next) => {
    const answer = await asks.ask('question', {
      questions: Array.isArray(request.questions) ? request.questions : [],
    }, request.signal)
    // A withdrawn question is an aborted ask: the service maps the rejection to
    // its own aborted-question error because the request signal is aborted.
    if (answer?.withdrawn === true) throw new Error('user question was cancelled')
    if (answer?.ok === true && Array.isArray(answer.answers)) return { answers: answer.answers }
    return next()
  })
}
