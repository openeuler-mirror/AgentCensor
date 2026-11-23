import { realpath, stat, rm } from 'node:fs/promises'
import { isAbsolute, join, resolve } from 'node:path'
import { scrubbedParentEnv } from '@deepseek-ai/dsh-subprocess'
import { runProcess } from './censorfs-cli.js'
import { temporaryEnvironment } from './environment.js'
import { safeSegment } from './validation.js'

function eventWorkingDirectory(event) {
  const data = event?.data ?? {}
  const args = data.arguments ?? data.args ?? data.input ?? data.params ?? {}
  for (const value of [data.cwd, data.workingDirectory, args.cwd, args.workingDirectory, args.working_directory]) {
    if (typeof value === 'string' && isAbsolute(value)) return value
  }
  return undefined
}

export async function resolvePublishWorkspace(agent, configuredPath) {
  let candidate = configuredPath
  if (candidate === undefined) {
    const header = agent.session.header ?? {}
    candidate = header.workspacePath ?? header.workspace ?? header.cwd ?? agent.session.workspacePath ?? agent.session.workspace
  }
  if (candidate === undefined) {
    const events = agent.session.snapshotEvents?.() ?? []
    for (let i = events.length - 1; i >= 0 && candidate === undefined; i -= 1) {
      candidate = eventWorkingDirectory(events[i])
    }
  }
  if (typeof candidate !== 'string' || !isAbsolute(candidate)) {
    throw new Error('cannot determine the DSH workspace path; set publishWorkspacePath in the CensorFS plugin configuration')
  }
  const workspace = await realpath(resolve(candidate))
  const info = await stat(workspace)
  if (!info.isDirectory() || workspace === '/' || workspace === '/home' || workspace === '/root') {
    throw new Error(`refusing to materialize into an unsafe workspace path: ${workspace}`)
  }
  return workspace
}

export async function materializePublishedGeneration({
  cli, config, generationId, runId, variantId, workspace,
}) {
  const view = await cli.openGeneration(generationId)
  const tmpDir = join(config.tmpRoot, safeSegment(runId), safeSegment(variantId))
  try {
    const result = await runProcess(config.mounterCommand, [
      '--socket', config.socket,
      '--view-id', view.view_id,
      '--uid', String(view.owner_uid),
      '--gid', String(view.owner_gid),
      '--read-only',
      '--',
      'cp', '-a', '--no-preserve=ownership', '/workspace/.', `${workspace}${workspace.endsWith('/') ? '' : '/'}`,
    ], {
      cwd: config.controlPlaneCwd,
      env: { ...scrubbedParentEnv(), ...config.childEnv, ...temporaryEnvironment(tmpDir) },
      timeoutMs: config.materializeTimeoutMs ?? 120000,
      maxOutput: 1024 * 1024,
    })
    if (result.code !== 0) {
      throw new Error(result.stderr.trim() || result.stdout.trim() || `materialization exited ${result.code}`)
    }
  } finally {
    await cli.closeView(view.view_id).catch(() => undefined)
    await rm(tmpDir, { recursive: true, force: true }).catch(() => undefined)
  }
  // Only the validation scratch directory owned by this run/variant is removed.
  return { workspace, generationId }
}
