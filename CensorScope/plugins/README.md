# CensorScope × dsh 插件

本目录包含三个随 dsh 预装的插件（不修改 dsh 源码，只做 profile 级插件安装）：

| 包 | 版本 | 作用 | 装入 profile |
|---|---|---|---|
| `censorscope-host` | 0.1.0 | main 角色：单 trace track-add(trace-id 复用) + per-call cache writer + `/censorscope/call` 路由；worker 角色：注入 `DSH_CENSORSCOPE_CALL_ID` + call-start/end span（真实起止时刻） | web + headless |
| `agentcensor-session-proxy` | 0.1.0 | web 每会话 headless worker（agentcensor 拓扑）+ `CENSORSCOPE_SESSION_ID` | web |
| `censorscope-ui` | 0.1.0 | 浏览器会话「CensorScope」Tab（替换原生轨迹视图） | web |

## 一键安装（推荐）

插件变更必须 bump 版本并重新打包后才能生效（安装器按版本号刷新），请一律通过脚本安装：

```bash
export DSH_HOME=/path/to/dsh/home     # 必填，脚本本身不设置
./scripts/install-censorscope.sh all        # host(web+headless) + agentcensor-session-proxy(web) + ui(web)
```

模式说明：

- `all`：安装/刷新全部三个插件。
- `host`：只装 `censorscope-host`（web+headless）与 `agentcensor-session-proxy`（web），不装 `censorscope-ui`。

其它要点：

- 脚本幂等：版本未变时输出 `installed` 并跳过；版本变化时自动 `npm pack` 并用 `dsh plugin --profile <p> add` 刷新。
- 默认 `npm` 缓存不可用（如沙箱/无默认 pnpm store）时，可显式指定：
  ```bash
  export PNPM_STORE_DIR=<writable-dir> PNPM_CACHE_DIR=<writable-dir> NPM_CACHE_DIR=<writable-dir>
  ```
- 安装与 censorscoped/daemon 无关：装完插件后重启 dsh web 即生效；改动 eBPF 采集/daemon 侧仍需自行重建 daemon 并重启 censorscoped。
- 手动打包（等价于脚本内动作）：
  ```bash
  cd plugins/<pkg> && npm pack --pack-destination . --cache <工作区 .cache/npm>
  ```
