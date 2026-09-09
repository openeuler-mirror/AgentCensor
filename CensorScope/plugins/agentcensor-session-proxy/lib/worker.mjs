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
 * Everything else on stdout/stderr is host log passthrough.
 *
 * ── Stdin protocol (host → worker, one JSON per line) ──
 *   {"type":"message","message":{content,source,id?,role?}}   original user message
 *   {"type":"shutdown"}                                       graceful exit
 * stdin EOF ⇒ graceful exit (host process is gone).
 *
 * Env contract (set by the host driver — task-channel plumbing only, no
 * credential/model/configuration values):
 *   AGENTCENSOR_SESSION_ID   durable session identity to create or resume
 *   AGENTCENSOR_MODE         'create' (fresh) | 'resume' (existing durable log)
 * The worker runs with DSH_HOME = the HOST's home (config/credentials are read
 * in place, like stock dsh); only its writable data roots are private.
 */
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

  const censorscope = process.env.CENSORSCOPE_SESSION_ID ?? ''
  process.stdout.write(
    `agentcensor-worker resident session=${sessionId} mode=${mode} seq=${firstSeq} model=${selection.provider}/${selection.model}${censorscope.length > 0 ? ` censorscopeSession=${censorscope}` : ''}\n`,
  )
  process.stdout.write(`ACREADY\t${JSON.stringify({
    mode,
    firstSeq,
    model: { provider: selection.provider, model: selection.model },
  })}\n`)

  return controlLoop(agent, sessions)
}

/** Serve messages over stdin until a shutdown frame or EOF. */
async function controlLoop(agent, sessions) {
  let chain = Promise.resolve()
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
      chain = chain
        .then(() => runOneTurn(agent, sessions, message))
        .catch((error) => {
          // Runner-level turn failure: report and stay alive for the next
          // message. Model-level failures are handled natively inside the
          // agent loop and surface as ordinary session events (replayed).
          process.stdout.write(`ACERR\t${JSON.stringify({ message: error?.message ?? String(error) })}\n`)
        })
    } else if (frame?.type === 'shutdown') {
      break
    } else {
      process.stderr.write(`agentcensor-worker: unknown stdin frame type=${String(frame?.type)}\n`)
    }
  }
  await chain.catch(() => undefined)
  await sessions.flush(agent.session)
  return 0
}

/** Execute one user message, then report idle (agent finished the turn). */
async function runOneTurn(agent, sessions, message) {
  agent.followup(message)
  await agent.whenIdle()
  await sessions.flush(agent.session)
  process.stdout.write(`ACIDLE\t${JSON.stringify({ seq: agent.session.seq })}\n`)
}

/** Whether a value looks like a UserMessage the native followup accepts. */
function isUserMessage(value) {
  return value !== null && typeof value === 'object'
    && Array.isArray(value.content) && value.source !== undefined && typeof value.source === 'object'
}
