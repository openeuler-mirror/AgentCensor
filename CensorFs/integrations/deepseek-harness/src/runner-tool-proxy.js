const METHODS = new Map([
  ['read', 'fs.read'],
  ['write', 'fs.write'],
  ['edit', 'fs.edit'],
  ['glob', 'fs.glob'],
  ['grep', 'fs.grep'],
  ['read_image', 'fs.read_image'],
  ['bash', 'process.run'],
  ['job_list', 'job.list'],
  ['job_output', 'job.output'],
  ['job_kill', 'job.kill'],
])

const CENSORFS_DELEGATION_TOOLS = new Set(['branch_explore_inprocess'])

const HOST_SAFE_TOOLS = new Set([
  'ask_user_question',
  'web_search',
  'skill',
  'todo_write',
  'get_goal',
  'create_goal',
  'update_goal',
  'cordis_inspect_list',
  'cordis_inspect_query',
  'cordis_inspect_self',
  'cordis_define',
  'cordis_run',
  'cordis_stop',
  'cordis_undefine',
])

const UNSUPPORTED_EXECUTION_TOOLS = new Set([
  'pwsh',
  'terminal_create',
  'terminal_write',
  'terminal_read',
  'terminal_resize',
  'terminal_signal',
  'terminal_close',
  'subagent',
  'subagent_fork',
  'workflow',
  'ralph',
])

async function materializeImage(ctx, remote) {
  const attachments = ctx.get('attachments')
  if (attachments === undefined) throw new Error('read_image is unavailable for CensorFS Runner agents: no attachment service is mounted')
  const data = Buffer.from(remote.dataBase64, 'base64')
  const ref = await attachments.saveImage({ data, mediaType: remote.mediaType, name: remote.name })
  return {
    path: remote.path,
    image: {
      attachmentId: ref.attachmentId,
      mediaType: ref.mediaType,
      bytes: ref.bytes,
      width: ref.width,
      height: ref.height,
      ...(ref.name === undefined ? {} : { name: ref.name }),
    },
  }
}

export function registerRunnerToolProxy(ctx, runnerManager) {
  ctx.on('tools/execute', async (exec, next) => {
    const record = runnerManager.get(exec.agent)
    if (record === undefined) return next()

    if (UNSUPPORTED_EXECUTION_TOOLS.has(exec.name)
      || exec.name.startsWith('mcp__') || exec.name === 'lsp') {
      throw new Error(`${exec.name} has no CensorFS execution-world adapter yet`)
    }
    if (HOST_SAFE_TOOLS.has(exec.name) || CENSORFS_DELEGATION_TOOLS.has(exec.name)) return next()

    const method = METHODS.get(exec.name)
    if (method === undefined) {
      throw new Error(`${exec.name} has no declared CensorFS execution-world adapter`)
    }

    const args = exec.arguments
    if (typeof args !== 'object' || args === null || Array.isArray(args)) throw new Error(`${exec.name} arguments must be an object`)
    if ((exec.name === 'write' || exec.name === 'edit' || exec.name === 'bash')
      && (args.sandbox_permissions !== undefined || args.justification !== undefined)) {
      throw new Error('sandbox escalation is unavailable for delegated CensorFS Runner agents')
    }
    const remote = await record.client.call(method, args, exec.signal)
    const value = exec.name === 'read_image' ? await materializeImage(ctx, remote) : remote
    return { isError: false, value, content: [] }
  })
}
