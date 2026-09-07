import test from 'node:test'
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { isAbsolute } from 'node:path'
import { resolveChildCommand } from '../src/child-command.js'

// 回归：FUSE 子命令经 argv 穿过 sudo env_reset，裸名字在重置后的系统 PATH
// 里找不到（mounter exec → Os { code: 2, NotFound }，worker 0.1s 死亡）。
// 裸名字必须解析为插件自带 bin/ 的绝对路径。

test('bare child command resolves to the bundled bin absolute path', () => {
  const resolved = resolveChildCommand('dsh-jsonrpc-agent')
  assert.ok(isAbsolute(resolved), 'must be an absolute path')
  assert.ok(resolved.endsWith('/bin/dsh-jsonrpc-agent') || resolved.endsWith('\\bin\\dsh-jsonrpc-agent'))
  assert.equal(existsSync(resolved), true)
})

test('explicit paths and undefined pass through unchanged', () => {
  assert.equal(resolveChildCommand('/opt/custom/agent'), '/opt/custom/agent')
  assert.equal(resolveChildCommand('./relative/agent'), './relative/agent')
  assert.equal(resolveChildCommand(undefined), undefined)
})

test('unknown bare name fails with an actionable error', () => {
  assert.throws(
    () => resolveChildCommand('no-such-agent'),
    /not on the sudo-reset PATH.*absolute path/,
  )
})

test('path traversal in a bare name is rejected', () => {
  assert.throws(() => resolveChildCommand('..'), TypeError)
})
