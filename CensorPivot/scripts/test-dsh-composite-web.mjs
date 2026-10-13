#!/usr/bin/env node
// Actual installed composite profiles + Chromium + local model + running native services.
// Usage: node scripts/test-dsh-composite-web.mjs /path/to/deepseek-harness
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { execFileSync, spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

const dsh = resolve(process.argv[2] ?? '/root/deepseek-harness')
const requireDsh = createRequire(join(dsh, 'package.json'))
const { chromium } = createRequire(join(dsh, 'apps/web/package.json'))('playwright')
const directory = mkdtempSync(join(tmpdir(), 'pivot-web-'))
const home = join(directory, 'home')
const workspace = join(directory, 'workspace')
const guardConfig = '/etc/censorguard/agentcensor.yaml'
const originalPolicy = '/etc/censorguard/policy.d/censorguard-dsh-default.yaml'
const group = 'censorguard-dsh-default'
mkdirSync(workspace)
cpSync(join(process.env.DSH_HOME ?? '/root/.dsh', 'profiles'), join(home, 'profiles'), { recursive: true })
console.log(`Artifacts: ${directory}`)
const readSecret = join(workspace, 'chat-read.txt')
const deleteSecret = join(workspace, 'chat-delete.txt')
writeFileSync(readSecret, 'acceptance-only protected read\n')
writeFileSync(deleteSecret, 'acceptance-only protected delete\n')
writeFileSync(join(workspace, 'public.txt'), 'public test content\n')
const transactionFiles = () => readdirSync('/var/lib/censorpivot/transactions').sort()
const beforeTransactions = transactionFiles()
const children = []
let browser
let requests = 0
let toolResults = []
const model = createServer(async (request, response) => {
  if (!request.url.endsWith('/chat/completions')) { response.writeHead(404).end(); return }
  const chunks = []
  for await (const data of request) chunks.push(data)
  const body = JSON.parse(Buffer.concat(chunks).toString())
  requests++
  response.writeHead(200, { 'content-type': 'text/event-stream' })
  const chunk = (delta, finish_reason = null) => `data: ${JSON.stringify({ id: 'pivot-web-test', object: 'chat.completion.chunk', created: 1, model: 'deepseek-flash', choices: [{ index: 0, delta, finish_reason }] })}\n\n`
  if (body.messages.some(message => message.role === 'user' && JSON.stringify(message.content).includes('AgentCensor conversation tool audit'))) {
    toolResults = body.messages.filter(message => message.role === 'tool')
    const probes = [
      { name: 'read', args: { file_path: readSecret } },
      { name: 'bash', args: { command: `cat '${readSecret}'`, description: 'Read protected fixture', workdir: workspace } },
      { name: 'bash', args: { command: `rm -f '${deleteSecret}'`, description: 'Delete protected fixture', workdir: workspace } },
    ]
    const probe = probes[toolResults.length]
    if (probe) {
      assert(body.tools.some(tool => tool.function.name === probe.name), `missing tool ${probe.name}`)
      response.end(chunk({ role: 'assistant', tool_calls: [{ index: 0, id: `pivot-probe-${toolResults.length}`, type: 'function', function: { name: probe.name, arguments: JSON.stringify(probe.args) } }] }) + chunk({}, 'tool_calls') + 'data: [DONE]\n\n')
      return
    }
    response.end(chunk({ role: 'assistant', content: 'AgentCensor conversation tool audit complete.' }) + chunk({}, 'stop') + 'data: [DONE]\n\n')
    return
  }
  response.end(chunk({ role: 'assistant', content: 'AgentCensor composite Web test reply.' }) + chunk({}, 'stop') + 'data: [DONE]\n\n')
})

function ctl(...args) {
  return JSON.parse(execFileSync('/usr/bin/censorguardctl', ['--config', guardConfig, ...args, '--json'], { encoding: 'utf8', timeout: 30000 }))
}
async function waitFor(check, message, timeout = 60000) {
  const end = Date.now() + timeout
  while (Date.now() < end) { const result = await check(); if (result) return result; await sleep(100) }
  throw new Error(message)
}

try {
  await new Promise((resolveListen, reject) => { model.once('error', reject); model.listen(0, '127.0.0.1', resolveListen) })
  const policy = join(directory, 'policy.yaml')
  writeFileSync(policy, readFileSync(originalPolicy, 'utf8') + `\n  - file deny+audit ${readSecret} [read]\n  - file deny+audit ${deleteSecret} [delete]\n`)
  ctl('policy', 'apply', '--policy', group, '--file', policy)
  // Machine-level overlay is shared by web and its real per-session headless worker.
  writeFileSync(join(home, 'cordis.patch.yml'), `
- id: llm-deepseek
  config:
    apiKeyEnv: AGENTCENSOR_WEB_TEST_API_KEY
    baseURL: http://127.0.0.1:${model.address().port}
`)
  const child = spawn(process.execPath, ['--import', requireDsh.resolve('tsx/esm'), join(dsh, 'apps/cli/src/bin.ts'), '--profile', 'web', '--port', '0', '--no-open'], {
    cwd: dsh, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, DSH_HOME: home, DSH_TELEMETRY_DISABLED: '1', AGENTCENSOR_WEB_TEST_API_KEY: 'unused-local-test-key' },
  })
  children.push(child)
  let log = ''
  for (const stream of [child.stdout, child.stderr]) stream.on('data', data => { log += data; writeFileSync(join(directory, 'host.log'), log) })
  const url = await waitFor(() => {
    assert.equal(child.exitCode, null, log.slice(-7000))
    return log.match(/dsh web: (http:\/\/\S+)/)?.[1]
  }, 'composite Web did not become ready')
  assert(log.includes('attach 成功'), log)
  assert(!/patch: entry .+ not found/.test(log), log)
  assert(ctl('status').roots.includes(child.pid), 'Web host must be registered in Guard')
  browser = await chromium.launch(process.env.DSH_PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.DSH_PLAYWRIGHT_EXECUTABLE_PATH } : {})
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1680, height: 1000 } })
  page.setDefaultTimeout(30000)
  const errors = []
  page.on('pageerror', error => errors.push(String(error)))
  page.on('console', message => {
    writeFileSync(join(directory, 'console.log'), `${message.type()}: ${message.text()}\n`, { flag: 'a' })
  })
  page.on('response', async response => {
    if (response.request().method() === 'POST') {
      const body = await response.text().catch(() => '')
      writeFileSync(join(directory, 'api.log'), `${response.url().split('?')[0]} ${response.status()} ${body.slice(0, 3000)}\n`, { flag: 'a' })
    }
  })
  await page.goto(url)
  await page.getByRole('button', { name: '继续', exact: true }).click()
  await page.getByRole('button', { name: '设置', exact: true }).click()
  const settings = page.getByRole('dialog', { name: '设置', exact: true })
  await settings.getByRole('button', { name: '安全策略', exact: true }).click()
  await settings.getByText('已连接', { exact: true }).waitFor()
  assert((await settings.locator('textarea').inputValue()).includes('rules:'))
  await page.keyboard.press('Escape')
  console.log('PASS installed composite startup, actual Guard root registration and policy settings')
  await page.getByRole('textbox', { name: '选择工作区', exact: true }).click()
  const picker = page.getByRole('dialog', { name: '选择工作区目录' })
  await picker.getByRole('button', { name: '编辑路径', exact: true }).click()
  const path = picker.getByRole('textbox', { name: '编辑路径' })
  await path.fill(workspace)
  await path.press('Enter')
  await picker.getByRole('button', { name: '打开', exact: true }).click()
  const input = page.locator('[data-composer-input][contenteditable="true"]').first()
  await input.waitFor()
  await input.fill('AgentCensor composite Web smoke test')
  await page.getByRole('button', { name: '发送消息', exact: true }).click()
  await page.getByText('AgentCensor composite Web test reply.', { exact: true }).waitFor({ timeout: 60000 })
  assert(requests > 0)
  assert(log.includes('worker spawn ok'), 'conversation must use the installed Scope session worker')
  await page.getByRole('tab', { name: 'CensorScope', exact: true }).click()
  await page.getByText('AgentCensor composite Web smoke test', { exact: true }).first().waitFor()
  await page.getByRole('tab', { name: '安全拦截审计', exact: true }).click()
  await page.locator('[data-censorguard-audit][data-stream-state="live"]').waitFor()
  await page.getByRole('tab', { name: '对话', exact: true }).click()
  console.log('PASS real headless session worker, streamed conversation, Scope trajectory and live audit tabs')
  // Permit the isolated fixture to reach kernel enforcement rather than DSH approval refusal.
  await input.fill('/permission danger-full-access')
  await page.getByRole('button', { name: '发送消息', exact: true }).click()
  await page.getByRole('button', { name: /访问模式，当前：.*(完全|无限)/ }).waitFor()
  await input.fill('AgentCensor conversation tool audit')
  await page.getByRole('button', { name: '发送消息', exact: true }).click()
  await page.getByText('AgentCensor conversation tool audit complete.', { exact: true }).waitFor({ timeout: 60000 })
  writeFileSync(join(directory, 'tool-results.json'), JSON.stringify(toolResults, null, 2))
  assert.equal(toolResults.length, 3)
  assert(toolResults.every(result => /EPERM|Operation not permitted/.test(JSON.stringify(result))), toolResults)
  assert(existsSync(deleteSecret))
  await page.getByRole('tab', { name: '安全拦截审计', exact: true }).click()
  const audit = page.locator('[data-censorguard-audit]')
  await audit.locator('[data-audit-result="DENY"]').filter({ hasText: 'chat-read.txt' }).filter({ hasText: 'cat(' }).first().waitFor()
  await audit.locator('[data-audit-result="DENY"]').filter({ hasText: 'chat-delete.txt' }).filter({ hasText: 'rm(' }).first().waitFor()
  const idleBefore = Number(await audit.getAttribute('data-last-sequence'))
  await sleep(2000)
  const idleAfter = Number(await audit.getAttribute('data-last-sequence'))
  assert(idleAfter - idleBefore < 200, `event feedback flood: ${idleBefore}->${idleAfter}`)
  console.log('PASS actual conversation read/cat/rm EPERM, preserved file and matching browser DENY audit')
  await page.getByRole('tab', { name: '对话', exact: true }).click()
  await page.getByRole('button', { name: '打开右侧边栏', exact: true }).click()
  const files = page.locator('[data-files-state="tree"]')
  await files.waitFor()
  await files.getByRole('button', { name: 'public.txt', exact: true }).click()
  await page.getByText('public test content', { exact: false }).waitFor()
  console.log('PASS native sidebar file preview')
  // Host command/configuration events interleave with worker events. A later
  // turn must still use the same worker and a contiguous worker cursor.
  await input.fill('AgentCensor composite followup regression')
  await page.getByRole('button', { name: '发送消息', exact: true }).click()
  await page.getByText('AgentCensor conversation tool audit complete.', { exact: true }).nth(1).waitFor({ timeout: 60000 })
  assert.equal((log.match(/worker spawn ok/g) ?? []).length, 1, 'followup must reuse the resident worker')
  assert(!/mirror gap|synchronization sequence mismatch|mirror append failed/.test(log), log.slice(-7000))
  console.log('PASS permission synchronization and later conversation without worker sequence gaps')
  await waitFor(() => log.includes('trace'), 'Scope host did not establish a trace')
  writeFileSync(join(directory, 'browser.txt'), await page.locator('body').ariaSnapshot())
  await page.screenshot({ path: join(directory, 'browser.png'), fullPage: true })
  assert.deepEqual(errors, [])
  assert.deepEqual(transactionFiles(), beforeTransactions, 'unexpected native Pivot transaction during component-only DSH conversation')
  console.log('VERIFIED LIMIT: DSH conversations did not submit native Pivot BatchRequest transactions')
  console.log('PASS composite DSH Web acceptance')
} catch (error) {
  if (browser) for (const context of browser.contexts()) for (const page of context.pages()) {
    writeFileSync(join(directory, 'failure.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'))
    await page.screenshot({ path: join(directory, 'failure.png'), fullPage: true }).catch(() => {})
  }
  throw error
} finally {
  await browser?.close()
  for (const child of children.reverse()) {
    if (child.exitCode !== null || child.signalCode !== null) continue
    process.kill(-child.pid, 'SIGTERM')
    await Promise.race([new Promise(resolveExit => child.once('exit', resolveExit)), sleep(10000)])
    if (child.exitCode === null && child.signalCode === null) process.kill(-child.pid, 'SIGKILL')
  }
  ctl('policy', 'apply', '--policy', group, '--file', originalPolicy)
  model.closeAllConnections()
  await new Promise(resolveClose => model.close(resolveClose))
}
