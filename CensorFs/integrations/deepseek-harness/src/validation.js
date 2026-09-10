import { chmod, mkdir, chown } from 'node:fs/promises'
import { join } from 'node:path'
import { scrubbedParentEnv } from '@deepseek-ai/dsh-subprocess'
import { runProcess } from './censorfs-cli.js'
import { temporaryEnvironment } from './environment.js'

export function safeSegment(value) {
  return String(value).replaceAll(/[^a-zA-Z0-9_.-]/gu, '_').slice(0, 96)
}

export async function prepareVariantTmp(root, runId, variantId, uid, gid) {
  const runPath = join(root, safeSegment(runId))
  const path = join(runPath, safeSegment(variantId))
  await mkdir(root, { recursive: true, mode: 0o711 })
  await chmod(root, 0o711)
  await mkdir(runPath, { recursive: true, mode: 0o711 })
  await chmod(runPath, 0o711)
  await mkdir(path, { recursive: true, mode: 0o700 })
  try {
    await chown(path, uid, gid)
  } catch (error) {
    if (error?.code !== 'EPERM') throw error
  }
  return path
}

export function resolveValidationProfile(profiles, name) {
  const checks = profiles?.[name]
  if (!Array.isArray(checks) || checks.length === 0) {
    throw new Error(`unknown or empty validation profile: ${name}`)
  }
  return checks.map((check, index) => {
    if (typeof check?.name !== 'string' || check.name.length === 0) {
      throw new Error(`validation check ${index} has no name`)
    }
    if (typeof check.command !== 'string' || check.command.length === 0) {
      throw new Error(`validation check ${check.name} has no command`)
    }
    if (!Array.isArray(check.args) || !check.args.every((arg) => typeof arg === 'string')) {
      throw new Error(`validation check ${check.name} args must be strings`)
    }
    const timeoutMs = check.timeoutMs ?? 60000
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) {
      throw new Error(`validation check ${check.name} timeoutMs must be positive`)
    }
    return { ...check, timeoutMs, required: check.required !== false }
  })
}

// 在 Candidate 只读 View 里跑验证
export async function validateCandidate({
  cli,
  candidateId,
  checks,
  mounterCommand,
  socket,
  controlPlaneCwd,
  childEnv,
  tmpDir,
  signal,
}) {
  const results = []
  for (const check of checks) {
    let view
    const started = Date.now()
    try {
      view = await cli.openCandidate(candidateId)
      const execution = await runProcess(mounterCommand, [
        '--socket', socket,
        '--view-id', view.view_id,
        '--uid', String(view.owner_uid),
        '--gid', String(view.owner_gid),
        '--read-only',
        '--',
        check.command,
        ...check.args,
      ], {
        cwd: controlPlaneCwd,
        env: {
          ...scrubbedParentEnv(),
          ...childEnv,
          ...temporaryEnvironment(tmpDir),
        },
        timeoutMs: check.timeoutMs,
        signal,
        maxOutput: 1024 * 1024,
      })
      results.push({
        name: check.name,
        required: check.required,
        passed: execution.code === 0,
        exitCode: execution.code,
        durationMs: Date.now() - started,
        stdout: execution.stdout.slice(-32768),
        stderr: execution.stderr.slice(-32768),
      })
    } catch (error) {
      results.push({
        name: check.name,
        required: check.required,
        passed: false,
        durationMs: Date.now() - started,
        error: error instanceof Error ? error.message : String(error),
      })
    } finally {
      if (view !== undefined) await cli.closeView(view.view_id).catch(() => undefined)
    }
  }
  return {
    checks: results,
    requiredPassed: results.every((check) => !check.required || check.passed),
    passed: results.every((check) => check.passed),
  }
}