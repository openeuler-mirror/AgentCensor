/**
 * Worker-side glue for the CensorPivot composite.
 *
 * Session-proxy workers are siblings of the web host rather than children of
 * it.  CensorScope therefore cannot discover them through the web host's
 * process tree alone.  Attach each proxied worker to the already-created
 * dsh-main trace so eBPF events produced by bash/read/write tools are eligible
 * for the same per-call attribution as the web host.
 */
import { execFile } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join } from 'node:path'

export const name = 'censorpivot-headless'
export const inject = []

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

async function readTraceId() {
  const home = process.env.DSH_HOME || join(homedir(), '.dsh')
  try {
    const record = JSON.parse(await readFile(join(home, '.censorscope', 'trace.json'), 'utf8'))
    return typeof record?.traceId === 'string' && record.traceId !== '' ? record.traceId : null
  } catch {
    return null
  }
}

function attachWorker(traceId) {
  // Workers deliberately receive no host credentials or model configuration.
  // This fixed, sudo-restricted control wrapper is the one deployment-specific
  // capability needed to inspect the worker namespace.
  const ctl = process.env.CENSORSCOPE_CTL || '/usr/local/bin/censorscopectl-dsh'
  return new Promise((resolve) => {
    execFile(
      ctl,
      ['--json', 'track-add', '--root-pid', String(process.pid), '--name', 'dsh-session-worker', '--trace-id', traceId],
      { timeout: 40_000, windowsHide: true },
      (error, stdout, stderr) => resolve({ ok: error === null, detail: String(stdout || stderr || error?.message || '').trim() }),
    )
  })
}

async function attachProxiedWorker() {
  if (!process.env.AGENTCENSOR_SESSION_ID) return
  // The main web host writes trace.json asynchronously during startup.  A
  // fresh worker can precede it, so retry without holding worker boot open.
  for (let attempt = 0; attempt < 12; attempt += 1) {
    const traceId = await readTraceId()
    if (traceId) {
      const result = await attachWorker(traceId)
      if (result.ok) {
        process.stderr.write(`[censorpivot-headless] worker trace attached traceId=${traceId} pid=${process.pid}\n`)
        return
      }
      if (attempt === 11) process.stderr.write(`[censorpivot-headless] worker trace attachment failed: ${result.detail}\n`)
    }
    await sleep(1_000)
  }
}

export function apply() {
  void attachProxiedWorker()
}
export default { name, inject, apply }
