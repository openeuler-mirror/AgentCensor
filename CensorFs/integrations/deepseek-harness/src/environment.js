import { join } from 'node:path'

// 隔离子 Agent 的缓存和临时文件
export function temporaryEnvironment(tmpDir) {
  return {
    TMPDIR: tmpDir,
    TMP: tmpDir,
    TEMP: tmpDir,
    XDG_CACHE_HOME: join(tmpDir, 'xdg-cache'),
    CARGO_TARGET_DIR: join(tmpDir, 'cargo-target'),
    npm_config_cache: join(tmpDir, 'npm-cache'),
    PIP_CACHE_DIR: join(tmpDir, 'pip-cache'),
    PYTHONPYCACHEPREFIX: join(tmpDir, 'python-cache'),
  }
}