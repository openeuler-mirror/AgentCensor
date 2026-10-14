/**
 * Host half of the CensorPivot composite layer.  CensorScope remains the
 * collector; this plugin only exposes a bounded, cached read endpoint.
 */
import { execFile } from 'node:child_process'
import { mkdir, readFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join, resolve } from 'node:path'

export const name = 'censorpivot'
export const inject = []

const inFlight = new Map()
const EXPORT_TIMEOUT_MS = 60_000
const PAGE_SIZE = 500
const safeSegment = (value) => String(value).replace(/[^A-Za-z0-9._-]/g, '_')
const dshHome = () => process.env.DSH_HOME ?? join(homedir(), '.dsh')
const cachePath = (home, sessionId, callId) => resolve(home, '.censorpivot', 'scope-cache', safeSegment(sessionId), `${safeSegment(callId)}.json`)

async function readJson(path) {
  return JSON.parse(await readFile(path, 'utf8'))
}

async function exportCall(home, sessionId, callId) {
  const key = `${sessionId}:${callId}`
  if (inFlight.has(key)) return inFlight.get(key)
  const task = (async () => {
    const outPath = cachePath(home, sessionId, callId)
    try {
      const cached = await readJson(outPath)
      if (Array.isArray(cached.events) && cached.events.length > 0) return cached
    } catch {}
    await mkdir(join(home, '.censorpivot', 'scope-cache', safeSegment(sessionId)), { recursive: true })
    const record = await readJson(resolve(home, '.censorscope', 'trace.json'))
    if (typeof record?.traceId !== 'string' || record.traceId === '') throw new Error('CensorScope trace is unavailable')
    await new Promise((resolvePromise, reject) => {
      // The daemon runs with elevated privileges and trace visibility is
      // deliberately gated by the caller identity.  DSH supplies the
      // approved control wrapper through CENSORSCOPE_CTL; falling back keeps
      // standalone development installs working.
      execFile(process.env.CENSORSCOPE_CTL || 'censorscopectl', [
        '--json', 'export', '--trace-id', record.traceId, '--session-id', sessionId,
        '--call-id', callId, '--no-internal', '--page-size', String(PAGE_SIZE), '--out-path', outPath,
      ], { timeout: EXPORT_TIMEOUT_MS, windowsHide: true }, (error, stdout, stderr) => {
        if (error) {
          const detail = String(stderr || stdout || error.message || 'export failed').trim()
          reject(new Error(error.killed ? `timed out after ${EXPORT_TIMEOUT_MS}ms: ${detail}` : detail))
          return
        }
        resolvePromise()
      })
    })
    return readJson(outPath)
  })()
  inFlight.set(key, task)
  try { return await task } finally { inFlight.delete(key) }
}

function registerScopeRoute(ctx) {
  const home = dshHome()
  const handler = async (req, res) => {
    const json = (status, body) => {
      res.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' })
      res.end(JSON.stringify(body))
    }
    try {
      const url = new URL(req.url ?? '/', 'http://censorpivot.local')
      const sessionId = url.searchParams.get('session')
      const callId = url.searchParams.get('call')
      if (!sessionId || !callId) return json(400, { ok: false, error: 'session and call query params required' })
      const payload = await exportCall(home, sessionId, callId)
      const events = Array.isArray(payload.events) ? payload.events : []
      json(200, { ok: true, events, source: 'censorpivot-cache', limited: events.length >= PAGE_SIZE })
    } catch (error) {
      json(504, { ok: false, error: error?.message ?? String(error) })
    }
  }
  let attempts = 100
  const attach = () => {
    const server = ctx.get?.('webServer')
    if (!server) {
      if (--attempts > 0) setTimeout(attach, 100)
      return
    }
    try {
      server.register({ kind: 'exact', path: '/censorpivot/call', handler })
      ctx.logger?.info?.('CensorPivot Scope route registered at /censorpivot/call')
    } catch (error) {
      ctx.logger?.warn?.(`CensorPivot Scope route registration failed: ${error?.message ?? String(error)}`)
    }
  }
  attach()
}

export function apply(ctx) {
  ctx.logger?.info?.('CensorPivot composite enabled: CensorFS + CensorGuard + CensorScope')
  registerScopeRoute(ctx)
}

export default { name, inject, apply }
