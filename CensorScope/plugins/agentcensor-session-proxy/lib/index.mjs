/**
 * Session web factory: WebDriverAgent whose session executes in a RESIDENT dsh
 * WORKER process (one worker process per session, native agent-loop). The
 * worker is booted lazily on the session's first user message and then serves
 * every later message of that session as a followup on the same live agent;
 * it is NOT destroyed when a turn ends. The host replays the worker's session
 * events into the host session transparently — the "message → worker → events
 * back into the UI" closed loop. External plugin file; no dsh source changes.
 *
 * ── Why the HOST is the single durable writer ──
 * The browser follow/control streams are driven by host-side session appends
 * (ctx "session/event" on the ctx.sessions store), so worker output must be
 * appended into the host session to reach the UI live. The session-persistence
 * coordinator auto-persists EVERY appended live session (ctx.on('session/event')
 * → write-behind) and the JSONL backend has no cross-process write lock — so
 * the host appending mirrored events durably writes the shared log. Therefore
 * the worker MUST NOT write the shared log: the worker runs with a PRIVATE
 * persistence root (its overlay redirects the writable data roots out of the
 * shared host home) seeded with a snapshot copy of the durable session log; it
 * executes with the native agent-loop there and streams events back over
 * stdout. The host replays worker events 1:1 — worker is the content
 * authority; the host never synthesizes session data.
 *
 * ── Stdout protocol (worker → host, see worker.mjs) ──
 *   ACEVT\t<json>    one session event {type,seq,data,surfaceOp?,sourceEventSeqs?}
 *   ACREADY\t<json>  worker booted & ready for messages
 *   ACIDLE\t<json>   one turn finished (agent idle)
 *   ACERR\t<json>    runner-level turn failure (worker stays alive)
 *   ACCANCEL\t<json> cancel handed to the worker's native agent: {cause,keepInbox,seq,applied}
 *   ACASK\t<json>    approval/question ask forwarded by the worker: {id,kind,…payload}
 *   ACWITHDRAW\t<json> withdrawal of one forwarded ask: {id}
 *
 * ── Stdin protocol (host → worker, one JSON per line) ──
 *   {"type":"message","message":{content,source,id?,role?}}  original user message
 *   {"type":"cancel","cause":{kind},"keepInbox":bool}         native turn cancellation
 *   {"type":"answer","kind":…,"id":…,"ok":bool,…}             answer to a forwarded ask
 *   {"type":"shutdown"}                                      graceful exit
 *
 * ── Human interaction (approval + ask_user_question) ──
 * The browser is the only interactive answerer in dsh and it listens on THIS
 * host's remote waterfall, so a tool call running in the worker could never reach
 * a human: its ask fell through to the fail-closed outcome. The worker therefore
 * forwards such an ask (`ACASK`) and this host dispatches the same waterfall the
 * native web surface uses (see interaction-bridge.mjs) — control plane only, no
 * session data is written here; the audit pair and the tool result stay authored
 * by the worker and reach the UI through the ordinary 1:1 mirror.
 *
 * ── Cancellation (the stop button) ──
 * The web app's stop calls `agent.cancel({kind:'user'},{keepInbox:true})`; stock
 * dsh aborts the live turn inside the agent that owns it, so the transcript gets
 * an ordinary `turn/end {reason:{kind:'aborted',reason}}` plus the synthetic
 * tool/step closers, and the agent stays usable. This driver therefore FORWARDS
 * the cancel to the worker (which owns the native agent-loop) instead of killing
 * it: the worker aborts its live turn natively, those events stream back through
 * the ordinary 1:1 mirror, and the durable tail stays properly closed — which is
 * also keeps the host cursor aligned: the worker emits the normal closing events
 * before the next message is accepted, so the mirrored sequence remains contiguous.
 *
 * ── Worker env (host side injects at spawn: task-channel plumbing only) ──
 *   AGENTCENSOR_SESSION_ID    durable session identity (create or resume)
 *   AGENTCENSOR_MODE          'create' (fresh session) | 'resume' (existing durable log)
 *   CENSORSCOPE_SESSION_ID  always = the worker's session id
 * No credential/model/configuration value is forwarded: the worker boots with
 * DSH_HOME = the HOST home, so settings.yaml, .credentials.yaml, the
 * machine-level cordis.patch.yml and env layers read exactly as stock dsh;
 * the worker overlay redirects ONLY its writable data roots
 * (session-persistence/storage/attachments) into the per-worker private dir.
 *
 * Host-side env:
 *   AGENTCENSOR_DSH_BIN   dsh executable (default: auto-detect source tree, then 'dsh')
 *   DSH_ROOT              optional DeepSeek Harness source root for worker launch
 *   AGENTCENSOR_KEEP      '1' keeps per-worker private dirs for inspection
 */
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { promises as fsp } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createScope } from '@deepseek-ai/dsh-scope'
import { agentEvents } from '@deepseek-ai/dsh-agent'
import * as agentLoopModule from '@deepseek-ai/dsh-agent-loop'
import { createAssistantMessage, createUserMessage } from '@deepseek-ai/dsh-llm'
import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import { createInteractionBridge } from './interaction-bridge.mjs'

export const name = 'agentcensor-session-proxy'
export const inject = ['agents', 'sessions', 'sessionPersistence', 'sessionProjections']

const EVT_PREFIX = 'ACEVT\t'
const READY_PREFIX = 'ACREADY\t'
const IDLE_PREFIX = 'ACIDLE\t'
const ERR_PREFIX = 'ACERR\t'
const CANCEL_PREFIX = 'ACCANCEL\t'
const ASK_PREFIX = 'ACASK\t'
const WITHDRAW_PREFIX = 'ACWITHDRAW\t'
/** Grace granted to a resident worker after a shutdown frame before SIGTERM. */
const STOP_GRACE_MS = 3000
/** All live worker child processes, for host-exit cleanup. */
const ACTIVE_WORKERS = new Set()
/** Conventional exit codes for the host signals we reclaim workers on. */
const HOST_SIGNAL_EXIT_CODES = [
  ['SIGINT', 130],
  ['SIGTERM', 143],
  ['SIGHUP', 129],
  ['SIGQUIT', 131],
]
let hostExitHooksInstalled = false
let shuttingDown = false
/**
 * Host-side interaction bridge (approval + ask_user_question), created once with
 * the plugin's host context. See interaction-bridge.mjs: it is control plane
 * only and never writes session data.
 */
let interactionBridge

/**
 * Graceful stop of a set of workers: send the shutdown frame, then escalate to
 * SIGTERM after the grace period and SIGKILL shortly after (a resident dsh
 * worker may be mid-turn and ignoring signals; SIGKILL is the backstop that
 * guarantees no orphan survives the host).
 */
function stopWorkerChildren(children) {
  const signalAll = (sig) => {
    for (const child of children) {
      if (child.exitCode === null && child.signalCode === null) {
        try { child.kill(sig) } catch {}
      }
    }
  }
  for (const child of children) {
    if (child.exitCode === null && child.signalCode === null) {
      /* EPIPE etc. */
      try { child.stdin?.write(`${JSON.stringify({ type: 'shutdown' })}\n`) } catch {}
    }
  }
  const term = setTimeout(() => signalAll('SIGTERM'), STOP_GRACE_MS)
  const kill = setTimeout(() => signalAll('SIGKILL'), STOP_GRACE_MS + 1500)
  term.unref?.()
  kill.unref?.()
}

/** Stop every resident worker and exit; once per host shutdown, idempotent. */
function shutdownAllWorkers(exitCode) {
  if (shuttingDown) {
    process.exit(exitCode)
    return
  }
  shuttingDown = true
  const children = [...ACTIVE_WORKERS]
  process.stdout.write(`[agentcensor] host exit: stopping ${children.length} resident worker(s)\n`)
  if (children.length === 0) {
    process.exit(exitCode)
    return
  }
  const deadline = setTimeout(() => process.exit(exitCode), STOP_GRACE_MS + 2500)
  deadline.unref?.()
  let remaining = children.length
  const onClose = () => {
    remaining -= 1
    if (remaining <= 0) process.exit(exitCode)
  }
  for (const child of children) child.once('close', onClose)
  stopWorkerChildren(children)
}

function bundleDir() {
  return dirname(fileURLToPath(import.meta.url))
}

/** Resolve a worker launcher for installed dsh or a Harness source checkout. */
async function resolveDshWorkerLauncher() {
  if (process.env.AGENTCENSOR_DSH_BIN) {
    return { command: process.env.AGENTCENSOR_DSH_BIN, prefixArgs: [], label: process.env.AGENTCENSOR_DSH_BIN }
  }
  const candidates = [...new Set([process.env.DSH_ROOT, process.cwd()].filter(Boolean))]
  for (const candidate of candidates) {
    try {
      const pkg = JSON.parse(await fsp.readFile(join(candidate, 'package.json'), 'utf8'))
      if (pkg.scripts?.dsh) {
        return {
          command: 'pnpm',
          prefixArgs: ['--dir', candidate, 'dsh'],
          label: `pnpm --dir ${candidate} dsh`,
        }
      }
    } catch {
      // Continue to the installed command fallback.
    }
  }
  return { command: 'dsh', prefixArgs: [], label: 'dsh' }
}

/**
 * Worker overlay (real LLM only): headless profile minus its one-shot rows,
 * native agent-loop ON. The worker SHARES the host DSH_HOME for configuration
 * (settings.yaml, .credentials.yaml, machine-level cordis.patch.yml — whatever
 * stock dsh reads is read from the same home; no key/model is injected), so
 * this overlay only redirects the worker's WRITABLE data roots into the
 * per-worker private dir (it must never write the host's session store,
 * storages, or attachments).
 */
function workerOverlay(privHome, compression = 'none') {
  const runner = join(bundleDir(), 'worker.mjs')
  const privRoot = join(privHome, 'sessions')
  const lines = []
  // goal/schedule/subagent rows stay ENABLED: the worker model may use them
  // like in a normal headless run; their events land in the worker session and
  // mirror back into the host session. In-worker child sessions write the
  // worker's throwaway persistence root, so their durable logs do not outlive
  // the worker.
  lines.push(
    '- id: session-title-llm',
    '  disabled: true',
    '',
    '# plugin-package-inventory-deepseek attaches the dsh_plugin_packages request',
    '# extension (default on) and resolves every active loader row per request;',
    '# its prepare can throw in some environments -> "DeepSeek request extension',
    '# preparation failed" (REQUEST_EXTENSION) aborts the whole turn. Disabling it',
    '# in the worker drops only that diagnostic field; chat/tools unaffected.',
    '- id: plugin-package-inventory-deepseek',
    '  disabled: true',
    '',
    '- id: headless-startup',
    '  disabled: true',
    '',
    '- id: headless-runner',
    '  disabled: true',
    '',
    '# Private writable roots: the worker shares the host DSH_HOME for config',
    '# but must never write the host session store / storages / attachments.',
    '# Compression follows the shared artifact we seeded from (stock dsh web',
    '# persists .zstd; our none-packChunks overlays persist plain JSONL).',
    '- id: session-persistence-jsonl',
    '  config:',
    `    root: ${JSON.stringify(privRoot)}`,
    `    compression: ${compression === 'zstd' ? 'zstd' : 'none'}`,
    ...(compression === 'zstd' ? [] : ['    packChunks: false']),
    '',
    '- id: storage-json',
    '  config:',
    `    root: ${JSON.stringify(join(privHome, 'storages'))}`,
    '',
    '- id: attachment-local',
    '  config:',
    `    dshHome: ${JSON.stringify(privHome)}`,
    '',
    '- insert:',
  )
  lines.push('    - id: agentcensor-worker')
  lines.push(`      name: ${JSON.stringify(pathToFileURLish(runner))}`)
  // The worker composes the BASE agent plane (the `standard` agent preset, which
  // owns `tool-ask-user` / `ask_user_question`, is only mounted by the web
  // surface), so the model would have no way to ask the human anything. Insert
  // the tool row here; the interaction bridge carries the ask to the browser.
  lines.push('    - id: tool-ask-user')
  lines.push("      name: '@deepseek-ai/dsh-tool-ask-user'")
  return `${lines.join('\n')}\n`
}

function pathToFileURLish(path) {
  return `file://${path}`
}

/** Join the text blocks of a user message (for logs / failure notes). */
function messageText(message) {
  if (message === undefined || message === null || !Array.isArray(message.content)) return ''
  return message.content
    .filter((block) => block.type === 'text')
    .map((block) => block.text)
    .join('')
}

/** Host driver agent: one session ↔ one resident worker process. */
class WebDriverAgent {
  constructor(rootCtx, session, options) {
    this.id = session.id
    this.session = session
    this.options = options ?? {}
    this.status = 'idle'
    this.inbox = { nextTurn: [], nextStep: [] }
    this.dispatch = agentEvents(rootCtx, this)
    this.scope = createScope(rootCtx, this)
    this.ctx = this.scope.ctx.extend({ agent: this })
    this.rootCtx = rootCtx
    this.turn = 0
    // Resident worker lifecycle: workerState is undefined until the session's
    // first message lazily spawns the per-session worker process.
    this.workerState = undefined
    // In-flight spawn promise (ready).
    this.spawning = undefined
    // Messages forwarded to the worker, not yet answered by an IDLE.
    this.outstanding = 0
    // Messages accepted by the driver but not yet written to the worker's stdin;
    // a cancel racing one of these is held instead of dropped.
    this.unwritten = 0
    this.idleWaiters = []
    this.endSeedInserted = false
    this.workerStats = { mirrored: 0, skipped: 0, failed: 0 }
    this.sendChain = Promise.resolve()
    this.closedByHost = false
    // Cancels received while the worker could not accept them yet (spawn in
    // flight). Applied right after the next forwarded message so a stop that
    // races its own message still aborts that message's turn.
    this.pendingCancels = []
  }

  /** Handle one user message: route it to this session's worker process. */
  followup(message) {
    this.enqueueMessage(message)
    return undefined
  }

  steer(message) { this.followup(message) }
  send(message, _target, _wakeup) { this.followup(message) }
  inject() {}

  /**
   * Native-protocol cancellation (the stop button). Stock dsh aborts the live
   * turn inside the agent that owns it and keeps the agent usable, so the cancel
   * is FORWARDED to the resident worker — never a process kill. The worker's
   * native agent-loop then appends the ordinary cancellation events
   * (`tool/result` for started calls, `step/end`, `turn/end {kind:'aborted'}`),
   * which stream back through the same 1:1 mirror as any other turn output.
   * @param cause - caller intent ({kind:'user'} from the web stop button).
   * @param options - `keepInbox` preserves queued/steering messages (the web stop
   *   passes it, matching stock dsh).
   */
  cancel(cause, options) {
    const cancel = {
      cause: normalizeCancelCause(cause),
      keepInbox: options?.keepInbox === true,
    }
    interactionBridge?.disposeAgent(this, 'cancelled')
    const state = this.workerState
    if (state !== undefined && state.alive && !state.stopping) {
      process.stdout.write(
        `[agentcensor] cancel session=${this.id}: protocol cancel → worker pid=${String(state.child.pid ?? '?')} cause=${cancel.cause.kind} keepInbox=${cancel.keepInbox}\n`,
      )
      state.write({ type: 'cancel', cause: cancel.cause, keepInbox: cancel.keepInbox })
      return
    }
    if (this.outstanding === 0 && this.unwritten === 0) {
      // Nothing is running and nothing is queued: cancellation is a no-op and
      // must not arm the next turn (native semantics).
      process.stdout.write(`[agentcensor] cancel session=${this.id}: nothing to cancel (no live worker, no pending turn)\n`)
      return
    }
    // A message is queued behind a spawn that has not reached READY yet: hold the
    // cancel and flush it right after that message frame, so the worker starts
    // the turn and aborts it natively instead of losing the message.
    this.pendingCancels.push(cancel)
    process.stdout.write(`[agentcensor] cancel session=${this.id}: queued until the pending message reaches the worker\n`)
  }

  /** Resolves when every forwarded message has been answered (worker IDLE). */
  whenIdle() {
    if (this.outstanding === 0) return Promise.resolve()
    return new Promise((resolvePromise) => this.idleWaiters.push(resolvePromise))
  }

  async runMaintenance() {}

  emitStatus(status) {
    this.status = status
    try { this.dispatch.emit('agent/status', { status }) } catch (error) {
      process.stdout.write(`[agentcensor] status emit failed: ${String(error)}\n`)
    }
  }

  /** Queue one user message for this session's worker (spawning it lazily). */
  enqueueMessage(message) {
    const text = messageText(message)
    this.markBusy()
    this.emitStatus('running')
    this.unwritten += 1
    this.sendChain = this.sendChain
      .then(async () => {
        const state = await this.ensureWorker()
        if (state === undefined || !state.alive) throw new Error('worker unavailable')
        this.writeFrame(state, { type: 'message', message: forwardableMessage(message) })
        this.unwritten = Math.max(0, this.unwritten - 1)
        process.stdout.write(
          `[agentcensor] message → worker session=${this.id} text=${JSON.stringify(text.slice(0, 80))}\n`,
        )
        // A stop pressed while this message was still queued behind the spawn:
        // deliver it now so the worker opens the turn and aborts it natively.
        for (const cancel of this.pendingCancels.splice(0)) {
          process.stdout.write(
            `[agentcensor] cancel session=${this.id}: flushing queued protocol cancel cause=${cancel.cause.kind} keepInbox=${cancel.keepInbox}\n`,
          )
          this.writeFrame(state, { type: 'cancel', cause: cancel.cause, keepInbox: cancel.keepInbox })
        }
      })
      .catch((error) => {
        // Spawn/boot failure before the first mirrored event: record it as a
        // failure note so the UI and durable log stay sane. A cancel held for
        // this message dies with it (its turn never started).
        this.unwritten = Math.max(0, this.unwritten - 1)
        this.pendingCancels = []
        this.markDone()
        if (this.outstanding === 0) this.noteRunError(error, text)
      })
    return undefined
  }

  /** Lazily spawn (once) the resident worker for this session; resolves ready. */
  ensureWorker() {
    if (this.workerState !== undefined && this.workerState.alive) return this.workerState.ready
    if (this.spawning !== undefined) return this.spawning
    this.spawning = (async () => {
      const state = await this.spawnWorker()
      return state
    })().finally(() => {
      this.spawning = undefined
    })
    return this.spawning
  }

  /**
   * Spawn the per-session resident worker: private scratch dir with a seeded
   * log copy + overlay + the writable roots the overlay redirects into it,
   * DSH_HOME = the HOST home for config (credentials/settings/home patch/env
   * are read like stock dsh — nothing is resolved or injected by the driver).
   * Resolves once the worker announces READY; rejects on spawn/boot failure.
   */
  async spawnWorker() {
    const sessions = this.rootCtx.get('sessions')
    const persistence = this.rootCtx.get('sessionPersistence')
    if (sessions === undefined || persistence === undefined) {
      throw new Error('agentcensor: sessions/sessionPersistence unavailable')
    }
    const dshBin = process.env.AGENTCENSOR_DSH_BIN ?? 'dsh'

    await sessions.flush(this.session)
    this.endSeedInserted = false
    this.workerStats = { mirrored: 0, skipped: 0, failed: 0 }
    this.pendingCancels = []
    const header = this.session.header
    const located = persistence.locate?.(header)
    const realFile = located?.path
    const resume = realFile !== undefined && await fileHasEvents(realFile)

    const privHome = await fsp.mkdtemp(join(tmpdir(), 'agentcensor-worker-'))
    const privRoot = join(privHome, 'sessions')
    await fsp.mkdir(privRoot, { recursive: true })
    let seeded = false
    if (resume && realFile !== undefined) {
      await seedLogCopy(realFile, privRoot)
      seeded = true
    }
    const compression = typeof realFile === 'string' && realFile.endsWith('.zstd') ? 'zstd' : 'none'
    const overlayPath = join(privHome, 'overlay.yml')
    await fsp.writeFile(overlayPath, workerOverlay(privHome, compression), 'utf8')

    const hostHome = resolveDshHome(undefined, process.env)
    const childEnv = {
      ...process.env,
      DSH_HOME: hostHome,
      DSH_AGENTS_HOME: join(privHome, '.agents'),
      DSH_TELEMETRY_DISABLED: '1',
      AGENTCENSOR_SESSION_ID: this.session.id,
      AGENTCENSOR_MODE: resume ? 'resume' : 'create',
      CENSORSCOPE_SESSION_ID: this.session.id,
    }
    process.stdout.write(`[agentcensor] worker shares host home DSH_HOME=${hostHome} (no key/model env)\n`)

    const cwd = header.cwd ?? process.cwd()
    process.stdout.write(
      `[agentcensor] worker spawn mode=${resume ? 'resume' : 'create'} session=${this.id} pid=PENDING\n`,
    )
    process.stdout.write(`[agentcensor] worker launcher=${dshLauncher.label}\n`)
    const child = spawn(dshLauncher.command, [
      ...dshLauncher.prefixArgs,
      '--profile', 'headless', '--patch', overlayPath,
    ], {
      cwd,
      env: childEnv,
      stdio: ['pipe', 'pipe', 'pipe'],
    })
    process.stdout.write(`[agentcensor] worker spawn ok mode=${resume ? "resume" : "create"} pid=${String(child.pid ?? "?")} seed=${seeded ? "copied" : "none"}\n`)
    ACTIVE_WORKERS.add(child)

    let readyResolve = () => undefined
    let readyReject = () => undefined
    const ready = new Promise((resolvePromise, rejectPromise) => {
      readyResolve = resolvePromise
      readyReject = rejectPromise
    })
    let readySettled = false
    const settleReady = (ok, reason) => {
      if (readySettled) return
      readySettled = true
      if (ok) readyResolve(state)
      else readyReject(reason instanceof Error ? reason : new Error(String(reason)))
    }
    const state = {
      child,
      privHome,
      mode: resume ? 'resume' : 'create',
      alive: true,
      stopping: false,
      ready,
      closeResolve: undefined,
      stderrTail: [],
      write: (frame) => {
        if (!state.alive || state.stopping) return
        /* EPIPE etc. */
        try { child.stdin.write(`${JSON.stringify(frame)}\n`) } catch {}
      },
      stop: (signal) => {
        if (!state.alive || state.stopping) return
        if (signal === 'shutdown') {
          /* already gone */
          try { child.stdin.write(`${JSON.stringify({ type: 'shutdown' })}\n`) } catch {}
          state.stopping = true
          const timer = setTimeout(() => {
            /* already gone */
            try { child.kill('SIGTERM') } catch {}
            const kill = setTimeout(() => {
              /* already gone */
              try { child.kill('SIGKILL') } catch {}
            }, 1500)
            if (kill.unref !== undefined) kill.unref()
          }, STOP_GRACE_MS)
          if (timer.unref !== undefined) timer.unref()
        } else {
          state.stopping = true
          /* already gone */
          try { child.kill(signal) } catch {}
        }
      },
    }
    this.workerState = state
    state.close = new Promise((resolvePromise) => { state.closeResolve = resolvePromise })

    // stderr passthrough (tail kept for boot-failure reporting).
    child.stderr.on('data', (chunk) => {
      const textChunk = String(chunk)
      if (state.stderrTail.length < 64) state.stderrTail.push(textChunk)
      process.stdout.write(textChunk)
    })

    let spawnError = undefined
    child.once('error', (error) => {
      spawnError = error
      process.stdout.write(`[agentcensor] worker spawn error: ${String(error)}\n`)
      settleReady(false, error)
    })

    const consume = (async () => {
      const rl = createInterface({ input: child.stdout })
      for await (const line of rl) {
        if (line.startsWith(EVT_PREFIX)) {
          let event
          try {
            event = JSON.parse(line.slice(EVT_PREFIX.length))
          } catch (error) {
            process.stdout.write(`[agentcensor] unparsable worker event: ${String(error)}\n`)
            continue
          }
          this.appendMirrored(event, this.workerStats)
        } else if (line.startsWith(READY_PREFIX)) {
          let payload = {}
          /* keep {} */
          try { payload = JSON.parse(line.slice(READY_PREFIX.length)) } catch {}
          settleReady(true, undefined)
          process.stdout.write(
            `[agentcensor] worker ready mode=${payload.mode ?? state.mode} pid=${String(child.pid ?? '?')} firstSeq=${String(payload.firstSeq ?? '?')} model=${payload.model?.provider ?? '?'}/${payload.model?.model ?? '?'}\n`,
          )
        } else if (line.startsWith(IDLE_PREFIX)) {
          this.onTurnIdle()
        } else if (line.startsWith(ASK_PREFIX)) {
          let payload = {}
          /* keep {} */
          try { payload = JSON.parse(line.slice(ASK_PREFIX.length)) } catch {}
          const agent = this
          void Promise.resolve()
            .then(() => interactionBridge?.ask(agent, payload, (frame) => state.write(frame)))
            .catch((error) => {
              process.stdout.write(`[agentcensor] interaction ask failed: ${String(error)}\n`)
            })
        } else if (line.startsWith(WITHDRAW_PREFIX)) {
          let payload = {}
          /* keep {} */
          try { payload = JSON.parse(line.slice(WITHDRAW_PREFIX.length)) } catch {}
          interactionBridge?.withdraw(this, payload.id, 'worker withdrew')
        } else if (line.startsWith(CANCEL_PREFIX)) {
          let payload = {}
          /* keep {} */
          try { payload = JSON.parse(line.slice(CANCEL_PREFIX.length)) } catch {}
          process.stdout.write(
            `[agentcensor] worker cancel session=${this.id} applied=${payload.applied ?? '?'} cause=${payload.cause?.kind ?? '?'} keepInbox=${String(payload.keepInbox ?? '?')} seq=${String(payload.seq ?? '?')}\n`,
          )
        } else if (line.startsWith(ERR_PREFIX)) {
          let payload = {}
          /* keep {} */
          try { payload = JSON.parse(line.slice(ERR_PREFIX.length)) } catch {}
          process.stdout.write(`[agentcensor] worker turn error: ${payload.message ?? '?'}\n`)
          this.markDone()
        } else {
          process.stdout.write(`[agentcensor-worker] ${line}\n`)
        }
      }
    })().catch((error) => {
      process.stdout.write(`[agentcensor] worker stdout consume failed: ${String(error)}\n`)
    })

    child.once('close', (code, signal) => {
      state.alive = false
      state.stopping = true
      ACTIVE_WORKERS.delete(child)
      consume.catch(() => undefined)
      state.closeResolve?.({ code, signal })
      const codeText = code === null ? `signal=${signal ?? '?'}` : `code=${code}`
      process.stdout.write(
        `[agentcensor] worker closed pid=${String(child.pid ?? '?')} ${codeText} mirrored=${this.workerStats.mirrored} skipped=${this.workerStats.skipped} failed=${this.workerStats.failed}\n`,
      )
      if (spawnError === undefined && !state.stoppingBeforeClose) {
        // Unexpected death (never asked to stop): a boot failure if READY was
        // never announced, otherwise the worker is gone mid-life.
        const stderrTail = state.stderrTail.join('').slice(-2048)
        settleReady(false, new Error(`worker exited ${codeText} before ready: ${stderrTail}`))
      } else {
        settleReady(true, undefined)
      }
      // A worker that is gone can no longer answer prompts: withdraw anything
      // still pending so the browser panel is dismissed and no wait survives.
      interactionBridge?.disposeAgent(this, 'worker closed')
      // Only mirrored events are durable: a dead worker answers nothing.
      if (this.outstanding > 0) this.markDone()
      if (this.workerState === state) {
        this.workerState = undefined
        this.endSeedInserted = false
      }
      if (process.env.AGENTCENSOR_KEEP === '1') {
        process.stdout.write(`[agentcensor] worker home kept: ${privHome}\n`)
      } else {
        fsp.rm(privHome, { recursive: true, force: true }).catch(() => undefined)
      }
    })
    state.stoppingBeforeClose = false
    // stop() flips this so a close caused by stop() is not treated as a crash.
    const origStop = state.stop
    state.stop = (signal) => {
      state.stoppingBeforeClose = true
      origStop(signal)
    }

    return state.ready.then(() => state)
  }

  markBusy() {
    if (this.outstanding === 0) {
      // Drop stale waiters (an earlier whenIdle already settled).
      this.idleWaiters = []
    }
    this.outstanding += 1
  }

  markDone() {
    this.outstanding = Math.max(0, this.outstanding - 1)
    if (this.outstanding === 0) {
      this.emitStatus('idle')
      const waiters = this.idleWaiters
      this.idleWaiters = []
      for (const waiter of waiters) waiter()
    }
  }

  writeFrame(state, frame) {
    if (state === undefined || !state.alive) return
    state.write(frame)
  }

  onTurnIdle() {
    this.markDone()
    const sessions = this.rootCtx.get('sessions')
    if (sessions !== undefined) {
      sessions.flush(this.session).catch((error) => {
        process.stdout.write(`[agentcensor] session flush failed: ${error?.message ?? String(error)}\n`)
      })
    }
  }

  /**
   * Record one worker boot failure as a NORMAL-shaped turn so the durable log
   * stays readable on the next boot. Only used when the worker never became
   * ready (spawn/boot failure); the user message would otherwise be lost.
   */
  noteRunError(error, userText) {
    const text = `[agentcensor] worker unavailable: ${error?.message ?? String(error)}`
    process.stdout.write(`[agentcensor] ${text}\n`)
    try {
      const turn = ++this.turn
      const step = 1
      this.session.append('turn/start', { turn })
      const alreadyEchoed = userText !== undefined && this.session.snapshotEvents().some((event) => {
        if (event.type !== 'user/message') return false
        const content = event.data?.content ?? []
        return Array.isArray(content)
          && content.filter((block) => block.type === 'text').map((block) => block.text).join('') === userText
      })
      if (userText !== undefined && userText.length > 0 && !alreadyEchoed) {
        this.session.append('user/message', createUserMessage({
          content: [{ type: 'text', text: userText }],
          source: { kind: 'user' },
        }), { surfaceOp: 'append' })
      }
      this.session.append('step/start', { turn, step })
      this.session.append('assistant/message', {
        turn,
        step,
        message: createAssistantMessage({
          content: [{ type: 'text', text }],
          source: {
            provider: this.options.provider ?? 'deepseek-official',
            model: this.options.model ?? 'deepseek-v4-flash',
          },
        }),
      }, { surfaceOp: 'append' })
      this.session.append('step/end', { turn, step })
      this.session.append('turn/end', { turn, reason: { kind: 'completed' } })
    } catch (noteError) {
      process.stdout.write(`[agentcensor] failure note append failed: ${String(noteError)}\n`)
    }
  }

  /** Gracefully stop the resident worker (dispose/session close). */
  stopWorker() {
    const state = this.workerState
    if (state === undefined) return
    this.closedByHost = true
    state.stop('shutdown')
    this.workerState = undefined
  }

  appendMirrored(event, stats) {
    if (event === undefined || typeof event.seq !== 'number' || typeof event.type !== 'string') {
      stats.failed += 1
      return
    }
    if (event.seq < this.session.seq) {
      // Already present in the host log (boot/policy events etc.).
      stats.skipped += 1
      return
    }
    if (event.seq > this.session.seq) {
      // A resume worker appends its own `session/end-seed` marker (Session
      // constructor) when the seeded log does not end with one, so its stream
      // legitimately starts one seq past the host's. Mirror that one-time +1
      // by inserting the marker on the host; any other gap is real divergence
      // and is refused.
      if (event.seq === this.session.seq + 1 && !this.endSeedInserted) {
        try {
          this.session.append('session/end-seed', {})
          this.endSeedInserted = true
          stats.mirrored += 1
          process.stdout.write(
            `[agentcensor] mirror inserted resume session/end-seed at seq=${String(event.seq - 1)}\n`,
          )
        } catch (error) {
          stats.failed += 1
          process.stdout.write(`[agentcensor] mirror end-seed append failed: ${error?.message ?? String(error)}\n`)
          return
        }
      } else {
        process.stdout.write(
          `[agentcensor] mirror gap: worker seq ${String(event.seq)} > host seq ${String(this.session.seq)} (type=${event.type})\n`,
        )
        stats.failed += 1
        return
      }
    }
    try {
      // The host assigns seq/time on append; retain every other envelope
      // field and fail loudly if a future dsh event cannot be represented.
      const { type, seq: _seq, time: _time, data, ...intent } = event
      const unsupportedIntent = Object.keys(intent).filter(
        (key) => key !== 'surfaceOp' && key !== 'sourceEventSeqs',
      )
      if (unsupportedIntent.length > 0) {
        stats.failed += 1
        process.stdout.write(
          `[agentcensor] mirror refused unsupported event envelope fields: ${unsupportedIntent.join(', ')} (type=${type})\n`,
        )
        return
      }
      const hasSurface = intent.surfaceOp !== undefined || intent.sourceEventSeqs !== undefined
      if (hasSurface) {
        this.session.append(type, data, {
          ...(intent.surfaceOp === undefined ? {} : { surfaceOp: intent.surfaceOp }),
          ...(intent.sourceEventSeqs === undefined ? {} : { sourceEventSeqs: intent.sourceEventSeqs }),
        })
      } else {
        this.session.append(type, data)
      }
      stats.mirrored += 1
    } catch (error) {
      stats.failed += 1
      process.stdout.write(
        `[agentcensor] mirror append failed seq=${String(event.seq)} type=${event.type}: ${error?.message ?? String(error)}\n`,
      )
    }
  }
}

/** Copy the identity-relevant fields of a user message for the worker. */
function forwardableMessage(message) {
  const snapshot = { content: message.content, source: message.source }
  if (message.id !== undefined) snapshot.id = message.id
  if (message.role !== undefined) snapshot.role = message.role
  return snapshot
}

/**
 * Normalize a cancellation cause to the closed set the native agent accepts
 * (`AgentCancelCause`). The stop button sends `{kind:'user'}`; hook causes keep
 * their reason text; anything unrecognized degrades to `{kind:'user'}` so a
 * cancel is never dropped.
 */
function normalizeCancelCause(cause) {
  if (cause !== null && typeof cause === 'object') {
    const kind = cause.kind
    if (kind === 'user' || kind === 'parent') return { kind }
    if (kind === 'hook' && typeof cause.reason === 'string') return { kind: 'hook', reason: cause.reason }
  }
  return { kind: 'user' }
}

/** Whether the durable artifact exists and contains at least one event. */
async function fileHasEvents(path) {
  try {
    const text = await fsp.readFile(path, 'utf8')
    return text.split('\n').filter(Boolean).length > 1
  } catch {
    return false
  }
}

/** Copy the shared durable log into the worker's private root, same layout. */
async function seedLogCopy(realFile, privRoot) {
  const sessionDir = dirname(realFile)
  const projectDir = dirname(sessionDir)
  // e.g. <encId>/session.jsonl
  const relFromProject = relative(projectDir, realFile)
  const privFile = join(privRoot, basename(projectDir), relFromProject)
  await fsp.mkdir(dirname(privFile), { recursive: true })
  await fsp.copyFile(realFile, privFile)
}

/** Publish one agent around a session, replicating the agent-loop order. */
async function publish(rootCtx, ownerCtx, session, options, source, parentAgent) {
  const agent = new WebDriverAgent(rootCtx, session, options)
  const detachSession = agent.ctx.sessions.enter(session)
  // dsh 0.1.5 removes the implicit ctx.agent accessor.
  let initiator
  try {
    initiator = rootCtx.agents.currentInitiator?.()
  } catch {}
  const detachAgent = rootCtx.agents.enter(agent, parentAgent ?? initiator)
  try {
    agent.ctx.sessions.announce(session)
    rootCtx.agents.announce(agent)
    agentEvents(rootCtx, agent).emit('agent/session-start', { source })
  } catch (error) {
    detachAgent?.()
    detachSession?.()
    throw error
  }
  const dispose = (async () => {
    // Fire-and-forget: the process exits on stdin EOF anyway.
    agent.stopWorker()
    try {
      detachAgent?.()
    } finally {
      detachSession?.()
      await agent.scope.dispose()
    }
  })
  return { agent, dispose }
}

export async function apply(ctx) {
  const persistence = ctx.get('sessionPersistence')
  interactionBridge = createInteractionBridge(ctx)
  // Re-register what the built-in agent-loop row used to provide for the UI
  // control stream (row is disabled by overlay). dsh 0.1.1 does not export
  // this optional definition, so keep the worker bridge usable without it.
  const turnBoundaryProjectionDefinition = agentLoopModule.turnBoundaryProjectionDefinition
  if (turnBoundaryProjectionDefinition === undefined) {
    process.stdout.write('[agentcensor] turnBoundary projection unavailable; compatibility mode\n')
  } else {
    try {
      ctx.sessionProjections.register(turnBoundaryProjectionDefinition)
      process.stdout.write('[agentcensor] turnBoundary projection registered\n')
    } catch (error) {
      process.stdout.write(`[agentcensor] projection register: ${String(error)}\n`)
    }
  }

  // Host-exit cleanup so no resident worker is orphaned. Node does not emit
  // the 'exit' event when the process dies on a default signal, so explicit
  // signal handlers drive the graceful shutdown frames (workers exit on the
  // frame or on stdin EOF once we go); the 'exit' hook enforces a SIGKILL
  // backstop for paths that terminate without going through the handlers.
  if (!hostExitHooksInstalled) {
    hostExitHooksInstalled = true
    process.on('exit', () => {
      for (const child of ACTIVE_WORKERS) {
        if (child.exitCode === null && child.signalCode === null) {
          /* already gone */
          try { child.kill('SIGKILL') } catch {}
        }
      }
    })
    for (const [signal, code] of HOST_SIGNAL_EXIT_CODES) {
      process.on(signal, () => shutdownAllWorkers(code))
    }
  }

  const factory = {
    async createAgent(ownerCtx, options) {
      const id = String(options.sessionId)
      process.stdout.write(`[agentcensor] createAgent id=${id}\n`)
      const session = ctx.sessions.prepare(id, { meta: options.meta ?? {} })
      let handle
      try {
        if (persistence !== undefined) {
          handle = await persistence.create(session.header, session.inheritedEventCount)
        }
      } catch (error) {
        process.stdout.write(`[agentcensor] persistence.create: ${error?.message ?? String(error)}\n`)
      }
      return publish(ctx, ownerCtx, session, options.agentOptions, 'startup', options.parentAgent)
    },
    async resume(ownerCtx, options) {
      const id = String(options.resumeSessionId)
      process.stdout.write(`[agentcensor] resume id=${id}\n`)
      if (persistence === undefined) throw new Error('agentcensor: no sessionPersistence')
      // dsh 0.1.5 exposes persistence through per-session handles. The
      // previous prepare(id) convenience API is no longer part of the
      // service, so reconstruct the live session from a read-only snapshot.
      const handle = await persistence.open(id, 'read')
      try {
        const snapshot = await handle.read(0)
        const session = ctx.sessions.prepare(id, {
          seed: snapshot.events,
          meta: structuredClone(handle.header),
          inheritedEventCount: handle.inheritedEventCount,
          eventState: snapshot.eventState,
        })
        return await publish(ctx, ownerCtx, session, options.agentOptions, 'resume', options.parentAgent)
      } finally {
        await handle.close()
      }
    },
  }
  try {
    ctx.agents.setFactory(factory)
    process.stdout.write('[agentcensor] setFactory OK\n')
  } catch (error) {
    process.stdout.write(`[agentcensor] setFactory FAILED: ${String(error)}\n`)
    process.exit(2)
  }
}
