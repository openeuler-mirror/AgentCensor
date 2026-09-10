import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

// FUSE 子命令经 argv 传给 mounter 并穿过 sudo env_reset —— 重置后的系统 PATH
// 里没有用户 bin 目录，裸名字 execvp 直接 ENOENT
// （mounter 报 Os { code: 2, kind: NotFound }）。裸名字因此解析为插件自带
// bin/ 下的绝对路径，部署自包含；显式绝对/相对路径原样保留。
export function resolveChildCommand(childCommand) {
  if (typeof childCommand !== 'string' || childCommand.length === 0 || childCommand.includes('/')) {
    return childCommand
  }
  if (!/^[A-Za-z0-9_.-]+$/u.test(childCommand)) {
    throw new TypeError(`dsh:explore childCommand "${childCommand}" must be an absolute path or a bundled bin name`)
  }
  const binDir = resolve(fileURLToPath(new URL('../bin/', import.meta.url)))
  const bundled = resolve(binDir, childCommand)
  // 纯点号名（"." / ".."）经 resolve 会逃逸出 bin/，必须拒绝。
  if (dirname(bundled) !== binDir) {
    throw new TypeError(`dsh:explore childCommand "${childCommand}" must be an absolute path or a bundled bin name`)
  }
  if (!existsSync(bundled)) {
    throw new TypeError(`dsh:explore childCommand "${childCommand}" is not on the sudo-reset PATH and no bundled bin/${childCommand} exists; set childCommand (or DSH_CENSORFS_CHILD_COMMAND) to an absolute path`)
  }
  return bundled
}
