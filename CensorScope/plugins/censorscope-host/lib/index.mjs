/**
 * censorscope-host: CensorScope observation integration plugin (no dsh source changes).
 *
 * Two roles in one plugin, chosen by the process context it loads into:
 *
 *  - worker role (a dsh headless process running an agent loop, typically the
 *    agentcensor-per-session worker; marker = AGENTCENSOR_SESSION_ID env):
 *    registers a ctx.shellEnv contributor that injects
 *    DSH_CENSORSCOPE_CALL_ID = <current tool call id> into every model shell
 *    (bash/pwsh) execution, so the whole forked process tree of that tool call
 *    is attributed by the eBPF collector;
 *
 *  - main role (the dsh process that owns the GUI/session host; when no
 *    AGENTCENSOR_SESSION_ID marker is present): idempotently creates ONE
 *    persistent trace over this process with `censorscopectl track-add`,
 *    reusing a stored trace id across restarts, and writes the shared trace-id
 *    file under $DSH_HOME/.censorscope/ for worker reads.
 *
 * The plugin never blocks boot: censorscoped may be absent (track-add is retried
 * in the background) — installing this plugin has no dependency on censorscoped.
 */
export const name = 'censorscope-host'

export const inject = []

import { execFile } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join, resolve } from 'node:path'

const CONTRIBUTOR = 'censorscope-host'
const CALL_ID_KEY = 'DSH_CENSORSCOPE_CALL_ID'
// Actual dispatch instant per tool call (ms), captured in the shellEnv
// contributor's resolve() — the earliest in-process signal that the tool is
// really starting. Call-start prefers it over event-arrival "now" because
// batched step commits append tool/call events only after execution.
const CALL_STARTS = new Map()

/** Deterministic identity for the single host trace, persisted across restarts. */
function traceIdFile(dshHome) {
  return resolve(dshHome, '.censorscope', 'trace.json')
}

function log(...args) {
  process.stderr.write(`[censorscope-host] ${args.join(' ')}\n`)
}

// Opt-in span-debug trace (CENSORSCOPE_DEBUG_SPAN=1): appends JSONL under
// $DSH_HOME/.censorscope/span-debug.jsonl so end/start trigger paths can be
// resolved without scraping worker stderr through the web host.
function debugSpan(kind, fields) {
  if (process.env.CENSORSCOPE_DEBUG_SPAN !== '1') return
  const dshHome = process.env.DSH_HOME ?? join(homedir(), '.dsh')
  const record = JSON.stringify({ t: Date.now(), kind, ...fields })
  void writeFile(join(dshHome, '.censorscope', 'span-debug.jsonl'), `${record}\n`, { flag: 'a' }).catch(() => {})
}

function isWorker() {
  return process.env.AGENTCENSOR_SESSION_ID !== undefined
}

function censorscopectl() {
  return process.env.CENSORSCOPE_CTL !== undefined && process.env.CENSORSCOPE_CTL !== ''
    ? process.env.CENSORSCOPE_CTL
    : 'censorscopectl'
}

function runCensorscopectl(args, opts = {}) {
  const timeoutMs = (typeof opts === 'number' ? opts : opts.timeoutMs) ?? 5000
  return new Promise((resolvePromise) => {
    execFile(
      censorscopectl(),
      ['--json', ...args],
      { timeout: timeoutMs, windowsHide: true },
      (error, stdout, stderr) => {
        const text = String(stdout || '').trim()
        if (error !== null) {
          // Distinguish an execFile deadline kill (empty output, no stderr —
          // the censorscoped control plane stalled) from a real rejection so later
          // diagnosis is not ambiguous.
          const why = error.killed
            ? `timed out after ${timeoutMs}ms`
            : error.code !== undefined && error.code !== null
              ? `exit code ${error.code}${error.signal ? ` (signal ${error.signal})` : ''}`
              : String(error.message || 'exit != 0')
          const detail = String(stderr || '').trim() || text
          resolvePromise({ ok: false, code: error.code, message: detail ? `${why}: ${detail}` : why })
          return
        }
        let parsed = null
        try {
          parsed = JSON.parse(text)
        } catch {}
        resolvePromise({ ok: true, text, json: parsed })
      },
    )
  })
}

/** Read trace-id record (main writes; worker reads). Never throws. */
export async function readTraceRecord(dshHome) {
  try {
    const raw = await readFile(traceIdFile(dshHome), 'utf8')
    const parsed = JSON.parse(raw)
    if (typeof parsed?.traceId === 'string' && parsed.traceId !== '') return parsed
  } catch {}
  return null
}

/**
 * Resolve the shared dsh-main trace: local record first, then the daemon's
 * trace list. The local file can lag (main writes it asynchronously right after
 * boot) or be missing after a manual DSH_HOME wipe while the daemon still owns
 * the trace — without the fallback, workers would stay span-disabled until a
 * full web restart. Best-effort rewrites the local file; never throws.
 */
export async function resolveTraceRecord(dshHome) {
  const local = await readTraceRecord(dshHome)
  if (local !== null) return local
  try {
    const res = await runCensorscopectl(['trace-list'], { timeoutMs: 5000 })
    const traces = res.ok && Array.isArray(res.json?.traces) ? res.json.traces : []
    const active = traces
      .filter((t) => t.display_name === 'dsh-main' && t.lifecycle_state === 'active')
      .sort((a, b) => b.trace_id - a.trace_id)
    const pick = active[0] ?? traces.find((t) => t.display_name === 'dsh-main')
    if (!pick || typeof pick.trace_id !== 'number') return null
    const record = {
      traceId: String(pick.trace_id),
      rootPid: typeof pick.root_pid === 'number' ? pick.root_pid : null,
      updatedAt: Date.now(),
    }
    try {
      await mkdir(join(dshHome, '.censorscope'), { recursive: true })
      await writeFile(traceIdFile(dshHome), JSON.stringify(record, null, 2), 'utf8')
    } catch {}
    return record
  } catch {
    return null
  }
}

/**
 * main role: one persistent trace over this process, retried in the background
 * so boot never waits on censorscoped.
 */
async function ensureHostTrace(ctx) {
  const dshHome = process.env.DSH_HOME ?? join(homedir(), '.dsh')
  const myPid = process.pid
  // ~ every 2.5s for ~15s, then give up quietly (retry on next boot)
  const attempts = 6
  for (let attempt = 1; attempt <= attempts; attempt++) {
    const doctor = await runCensorscopectl(['doctor'])
    if (!doctor.ok) {
      if (attempt === 1) log(`censorscoped not reachable yet (${doctor.message || doctor.code}); will retry, boot unaffected`)
      await new Promise((r) => setTimeout(r, 2500))
      continue
    }
    const record = await readTraceRecord(dshHome)
    const traceArg = record?.traceId ? ['--trace-id', record.traceId] : []
    // track-add snapshots and persists the whole dsh-main process tree; when
    // the daemon is busy draining a boot backlog the reply can take well over
    // the default 5s, so use the same 20s budget as export/span calls.
    const added = await runCensorscopectl(['track-add', '--root-pid', String(myPid), '--name', 'dsh-main', ...traceArg], 20000)
    if (!added.ok) {
      if (attempt === 1 || attempt === attempts) log(`track-add attempt ${attempt}/${attempts} failed: ${added.message}`)
      await new Promise((r) => setTimeout(r, 2500))
      continue
    }
    const numericId = added.json?.trace_id
    if (typeof numericId === 'number') {
      const traceId = String(numericId)
      try {
        await mkdir(join(dshHome, '.censorscope'), { recursive: true })
        await writeFile(
          traceIdFile(dshHome),
          JSON.stringify({ traceId, rootPid: myPid, updatedAt: Date.now() }, null, 2),
          'utf8',
        )
        log(`trace ready traceId=trace-${traceId} rootPid=${myPid} file=${traceIdFile(dshHome)}`)
      } catch (error) {
        log(`trace record write failed: ${error?.message ?? String(error)}`)
      }
    } else {
      log(`track-add ok but reply had no trace_id: ${added.text}`)
    }
    return
  }
}

/**
 * worker role: register the per-execution env contributor.
 * resolve() receives the current ToolExecution, so every model shell call gets
 * its own DSH_CENSORSCOPE_CALL_ID (parallel calls are therefore exact, no races).
 */
function registerCallEnv(ctx) {
  const shellEnv = ctx.get?.('shellEnv')
  if (shellEnv === undefined) {
    return false
  }
  try {
    const dispose = shellEnv.register({
      name: CONTRIBUTOR,
      variables: {
        [CALL_ID_KEY]: { description: 'CensorScope per-tool-call attribution id' },
      },
      resolve: (execution) => {
        const callId = execution?.callId
        if (callId === undefined) return {}
        const id = String(callId)
        try {
          CALL_STARTS.set(id, Date.now())
        } catch {}
        return { [CALL_ID_KEY]: id }
      },
    })
    ctx.effect?.(() => dispose, 'censorscope-host: shellEnv contributor')
    log(`worker role: DSH_CENSORSCOPE_CALL_ID contributor registered (${CALL_ID_KEY})`)
    return true
  } catch (error) {
    log(`worker role: contributor registration failed: ${error?.message ?? String(error)}`)
    return false
  }
}

/**
 * worker role: report tool-call spans to censorscoped (call-start / call-end).
 * Spans are strictly bound to one call id; the daemon uses them only as a
 * fallback for events that carry no DSH_CENSORSCOPE_CALL_ID env marker.
 *
 * Start rides the session/event 'tool/call' stream. End prefers the session
 * 'tool/result' row's OWN `time` (accurate commit instant, immune to late
 * delivery); the runtime 'tools/result' event remains only a fallback trigger
 * for paths that never produce a session row. A call-end without a matching
 * open span is a no-op daemon UPDATE, so sending it unconditionally is safe.
 */
function registerSpanEmitter(ctx) {
  const dshHome = process.env.DSH_HOME ?? join(homedir(), '.dsh')
  // Only an agent-loop host dispatches tools (worker under agentcensor, or the
  // single-process deployment); a web host that only mirrors worker events
  // never sees tools/result and therefore never opens spans, so its mirrored
  // tool/call stream stays inactive until a real dispatch is seen.
  let toolsSeen = false
  const started = new Set()
  // A process that executes tools under agentcensor is spawned with these
  // session env vars (web mirror hosts never carry them), so the session-row
  // end path can act from boot without waiting for the first runtime event.
  const workerHost = Boolean(
    process.env.CENSORSCOPE_SESSION_ID || process.env.AGENTCENSOR_SESSION_ID,
  )
  // Call id -> ended-at ns from the session 'tool/result' row's own `time`
  // (the runtime's commit instant, stamped at append). Reused no matter how
  // late the runtime 'tools/result' event is delivered, so a batch-deferred
  // first-call result cannot stretch the span window.
  const rowEnds = new Map()
  const closed = new Set()
  let record = null
  let resolving = null
  let lastResolveAttempt = 0
  let disabledLogged = false
  let firstFailureLogged = false
  const report = (label, res) => {
    if (res.ok) {
      log(`span: ${label} ok ${res.text}`)
    } else if (!firstFailureLogged) {
      firstFailureLogged = true
      log(`span: ${label} failed (${res.message ?? res.code}); further failures silent`)
    }
  }
  // Workers can boot before the main process has finished writing the shared
  // trace record, so a missing record must keep re-resolving (local file, then
  // daemon trace-list) — never permanently disable spans.
  const resolveTrace = async () => {
    if (record !== null) return record
    const now = Date.now()
    if (resolving !== null) return resolving
    if (now - lastResolveAttempt < 2000) return null
    lastResolveAttempt = now
    resolving = (async () => {
      record = await resolveTraceRecord(dshHome)
      if (record !== null) {
        if (disabledLogged) log(`span: shared trace ready traceId=${record.traceId} (re-resolved)`)
      } else if (!disabledLogged) {
        disabledLogged = true
        log('span: no shared trace record found; spans disabled (will retry)')
      }
      return record
    })()
    try {
      return await resolving
    } finally {
      resolving = null
    }
  }
  const sessionIdOf = (session) =>
    (typeof session?.header?.id === 'string' && session.header.id) ||
    process.env.CENSORSCOPE_SESSION_ID ||
    process.env.AGENTCENSOR_SESSION_ID ||
    null
  const timeNs = () => String(Date.now() * 1_000_000)
  // Prefer the real dispatch moment recorded by the shellEnv contributor;
  // fall back to now when the call never went through a shell resolve (for
  // example in-process file tools) or the record is already consumed.
  const startNsFor = (callId) => {
    const ms = CALL_STARTS.get(callId)
    if (typeof ms === 'number') {
      CALL_STARTS.delete(callId)
      return String(ms * 1_000_000)
    }
    return timeNs()
  }
  // Generous deadline plus one short-backoff retry: one slow or transient
  // failure must not silently strip every tool call of its span (and of its
  // syscall attribution). call-start/call-end are idempotent daemon upserts,
  // so a retry after an uncertain timeout is safe.
  const send = async (action, args) => {
    const rec = await resolveTrace()
    if (rec === null) return
    debugSpan('send', { action, callId: args.find((v, i) => args[i - 1] === '--call-id'), attempt: 1 })
    for (let attempt = 1; ; attempt++) {
      const res = await runCensorscopectl([action, '--trace-id', rec.traceId, ...args], { timeoutMs: 20000 })
      if (res.ok || attempt >= 2) {
        report(action, res)
        debugSpan('send-result', { action, ok: res.ok, at: attempt })
        return
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 500))
    }
  }
  const sendStart = async (session, event) => {
    if (!toolsSeen) return
    const callId = event?.data?.callId
    if (typeof callId !== 'string' || callId === '') return
    const sessionId = sessionIdOf(session)
    await send('call-start', [
      ...(sessionId ? ['--session-id', sessionId] : []),
      '--call-id', callId,
      '--pid', String(process.pid),
      '--started-at', startNsFor(callId),
    ])
  }
  // `endedNs` is authoritative: it is the session-row commit time when known
  // (see rowEnds), so a late runtime event cannot overwrite a correct window
  // with "now". A second close for the same call is a no-op.
  const closeSpan = async (callId, sessionId, endedNs, isError) => {
    if (typeof callId !== 'string' || callId === '') return
    if (closed.has(callId)) {
      debugSpan('close-skip', { callId, reason: 'already-closed' })
      return
    }
    closed.add(callId)
    debugSpan('close', { callId, endedNs, isError, hadStart: started.has(callId) })
    const status = isError ? 'error' : 'success'
    // Agent loops sometimes append all tool/call events only at step commit,
    // after this process has seen its first result; emit a synthetic start so
    // the span is closed instead of left dangling.
    if (!started.has(callId)) {
      started.add(callId)
      await send('call-start', [
        ...(sessionId ? ['--session-id', sessionId] : []),
        '--call-id', callId,
        '--pid', String(process.pid),
        '--started-at', startNsFor(callId),
      ])
    }
    started.delete(callId)
    await send('call-end', [
      ...(sessionId ? ['--session-id', sessionId] : []),
      '--call-id', callId,
      '--pid', String(process.pid),
      '--ended-at', endedNs,
      '--status', status,
    ])
  }
  const sendEnd = async (exec, result) => {
    const callId = exec?.callId
    if (typeof callId !== 'string' || callId === '') return
    toolsSeen = true
    const sessionId = sessionIdOf(exec?.agent)
    const isError = result?.isError === true || exec?.result?.isError === true || exec?.error !== undefined
    // Fallback only: prefer the row-derived ended-at (accurate) whenever the
    // session 'tool/result' row has already been observed.
    void closeSpan(callId, sessionId, rowEnds.get(callId) ?? timeNs(), isError)
  }
  ctx.on?.('session/event', (session, event) => {
    if (event?.type === 'tool/result') {
      if (!workerHost && !toolsSeen) return
      const callId = (typeof event?.data?.callId === 'string' && event.data.callId !== '')
        ? event.data.callId
        : (typeof event?.data?.message?.callId === 'string' && event.data.message.callId !== '')
            ? event.data.message.callId
            : (typeof event?.data?.message?.source?.callId === 'string' && event.data.message.source.callId !== '')
                ? event.data.message.source.callId
                : ''
      if (callId === '') return
      toolsSeen = true
      // The row's own time (ms, realtime) is the runtime's commit instant and
      // is valid no matter when this handler runs; convert to ns to match the
      // daemon's epoch-ns span/event clock (eBPF observed_at is realtime ns).
      const endedNs = String((typeof event?.time === 'number' ? event.time : Date.now()) * 1_000_000)
      rowEnds.set(callId, endedNs)
      debugSpan('row-end', { callId, rowMs: event?.time, endedNs })
      void closeSpan(callId, sessionIdOf(session), endedNs, event?.data?.message?.isError === true)
      return
    }
    if (event?.type === 'tool/call') {
      if (!toolsSeen) return
      const callId = event?.data?.callId
      if (typeof callId === 'string' && callId !== '') started.add(callId)
      void sendStart(session, event)
    }
  })
  ctx.on?.('tools/result', (execOrPayload, result) => {
    const exec = execOrPayload?.exec ?? execOrPayload
    debugSpan('runtime-end', { callId: exec?.callId })
    void sendEnd(exec, result)
  })
  // Close at the tool body's real completion: the session 'tool/result' row
  // and the 'tools/result' runtime event can both be held until a later commit
  // boundary (observed for the first call of a turn), leaving the span open
  // across a following short call. The closed guard makes a later row/runtime
  // end a no-op — its accurate ended-at remains a fallback for paths that
  // never reach the body.
  ctx.on?.('tools/execute', async (exec, next) => {
    const callId = exec?.callId
    debugSpan('execute-enter', { callId })
    const result = await next()
    debugSpan('execute-after', { callId, isError: result?.isError === true })
    if (typeof callId !== 'string' || callId === '' || closed.has(callId)) return result
    toolsSeen = true
    const endedNs = timeNs()
    rowEnds.set(callId, endedNs)
    void closeSpan(callId, sessionIdOf(exec?.agent), endedNs, result?.isError === true)
    return result
  })
  log('call-span emitter registered')
}

function safeSegment(value) {
  return String(value).replace(/[^A-Za-z0-9._-]/g, '_')
}

function callCachePath(dshHome, sessionId, callId) {
  return resolve(
    dshHome,
    '.censorscope',
    'cache',
    safeSegment(sessionId),
    `${safeSegment(callId)}.json`,
  )
}

async function callExportToCache(dshHome, sessionId, callId) {
  const rec = await resolveTraceRecord(dshHome)
  if (rec === null) return { ok: false, message: 'no trace record' }
  const out = callCachePath(dshHome, sessionId, callId)
  const res = await exportCallSnapshot(rec.traceId, sessionId, callId, out)
  return res.ok ? { ok: true, path: out } : { ok: false, message: res.message ?? res.code }
}

/**
 * `censorscopectl export` for one (session, call) into `outPath`, retried until
 * the snapshot carries events. The daemon's single-thread ingest can lag the
 * sqlite flush past the span close, so one immediate export would write a
 * premature EMPTY snapshot and the UI would report "no syscall data" forever.
 * Retrying with a short backoff lets the flush catch up. Never throws.
 */
async function exportCallSnapshot(traceId, sessionId, callId, outPath, opts = {}) {
  const attempts = opts.attempts ?? 4
  const waitMs = opts.waitMs ?? 1500
  let last = null
  for (let attempt = 1; attempt <= attempts; attempt++) {
    const res = await runCensorscopectl([
      'export',
      '--trace-id', traceId,
      '--session-id', sessionId,
      '--call-id', callId,
      '--no-internal',
      '--out-path', outPath,
    ], 20000)
    if (!res.ok) {
      last = res
      break
    }
    last = res
    let count = 0
    try {
      const parsed = JSON.parse(await readFile(outPath, 'utf8'))
      count = Array.isArray(parsed.events) ? parsed.events.length : 0
    } catch {
      // File unreadable/absent right after an ok export: stop retrying.
      count = 1
    }
    if (count > 0) break
    await new Promise((resolvePromise) => setTimeout(resolvePromise, waitMs))
  }
  return last
}

function registerCallRoute(ctx) {
  const dshHome = process.env.DSH_HOME ?? join(homedir(), '.dsh')
  const readCache = async (path) => {
    const { readFile } = await import('node:fs/promises')
    const text = await readFile(path, 'utf8')
    return JSON.parse(text)
  }
  const handler = async (req, res) => {
    const json = (code, body) => {
      res.writeHead(code, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' })
      res.end(JSON.stringify(body))
    }
    try {
      const url = new URL(req.url ?? '/', 'http://censorscope.local')
      const sessionId = url.searchParams.get('session')
      const callId = url.searchParams.get('call')
      if (!sessionId || !callId) {
        json(400, { ok: false, error: 'session and call query params required' })
        return
      }
      let payload = null
      let source = 'cache'
      let fromCache = true
      try {
        payload = await readCache(callCachePath(dshHome, sessionId, callId))
      } catch {
        fromCache = false
      }
      // An EMPTY cached snapshot is treated as stale: the daemon flushes on
      // one thread and the snapshot may have been exported before the events
      // landed. Fall back to a live DB export (which itself retries) so the
      // view always reflects what the daemon actually holds.
      if (!fromCache || !Array.isArray(payload.events) || payload.events.length === 0) {
        source = 'export'
        const exported = await callExportToCache(dshHome, sessionId, callId)
        if (!exported.ok) {
          if (fromCache && payload !== null) {
            // Live export failed but a cache exists: serve the cached snapshot
            // rather than an error (the record itself is valid).
            source = 'cache'
          } else {
            json(404, { ok: false, error: exported.message })
            return
          }
        } else {
          payload = await readCache(exported.path)
        }
      }
      const events = Array.isArray(payload.events) ? payload.events : []
      const counts = {}
      for (const event of events) {
        const key = event.kind_name || 'unknown'
        counts[key] = (counts[key] || 0) + 1
      }
      json(200, {
        ok: true,
        source,
        session: sessionId,
        call: callId,
        events,
        counts,
        segments: Array.isArray(payload.payload_segments) ? payload.payload_segments.length : 0,
      })
    } catch (error) {
      json(500, { ok: false, error: error?.message ?? String(error) })
    }
  }
  let attempts = 100
  const tryRegister = () => {
    attempts -= 1
    const server = ctx.get?.('webServer')
    if (server === undefined) {
      if (attempts > 0) {
        setTimeout(tryRegister, 100)
      } else {
        log('call route: webServer unavailable; syscall endpoint disabled')
      }
      return
    }
    try {
      server.register({ kind: 'exact', path: '/censorscope/call', handler })
      log('call route registered at /censorscope/call')
    } catch (error) {
      log(`call route register failed: ${error?.message ?? String(error)}`)
    }
  }
  tryRegister()
}

export function apply(ctx) {
  log(`loaded role=${isWorker() ? 'worker' : 'main'} dshHome=${process.env.DSH_HOME ?? '(default)'}`)
  // The ctx.shellEnv service may not exist yet when this plugin row applies
  // (row ordering / agent scoping), and reading ctx.shellEnv without declaring
  // an inject is denied by the cordis scope guard. Poll ctx.get('shellEnv') so
  // the contributor registers as soon as the service is available (~ up to 5s).
  const attempts = 50
  let attempt = 0
  let registered = false
  const tryRegister = () => {
    if (registered) return
    attempt += 1
    if (registerCallEnv(ctx)) {
      registered = true
      return
    }
    if (attempt < attempts) {
      setTimeout(tryRegister, 100)
    } else {
      log('ctx.shellEnv never became available; per-call env injection disabled')
    }
  }
  tryRegister()
  registerSpanEmitter(ctx)
  if (!isWorker()) {
    // main role runs in the background; censorscoped absence must never affect boot.
    void ensureHostTrace(ctx).catch((error) => {
      log(`track-add loop error: ${error?.message ?? String(error)}`)
    })
    // No automatic per-call cache writer: exports happen only when the UI
    // opens a call via /censorscope/call — a standing export reader on the
    // sqlite DB starves WAL checkpoints during capture.
    registerCallRoute(ctx)
  }
}
