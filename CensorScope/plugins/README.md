# CensorScope × dsh 插件

本目录提供三个通过 dsh profile 安装的 CensorScope 插件。

## 集成方式

- **会话观测**：每个 Web 会话使用独立 worker，通过 `CENSORSCOPE_SESSION_ID` 为
  worker 内执行的文件读写与子进程操作提供进程级会话身份。
- **会话同步**：宿主接收 worker 的根会话和子代理会话事件，供浏览器查看对话与会话树。
- **轨迹展示**：`censorscope-ui` 通过 profile patch 将原生轨迹入口替换为 CensorScope
  轨迹页，展示工具调用、事件详情和观测数据。

| 包 | 版本 | 作用 | 装入 profile |
|---|---|---|---|
| `censorscope-host` | 0.1.2 | main 角色：单 trace track-add(trace-id 复用；显式传递自身 PID namespace，兼容 Guard 的 ptrace 限制) + per-call cache writer + `/censorscope/call` 路由；worker 角色：注入 `DSH_CENSORSCOPE_CALL_ID` + call-start/end span（真实起止时刻） | web + headless |
| `agentcensor-session-proxy` | 0.1.4 | 每会话 headless worker + `CENSORSCOPE_SESSION_ID`；根会话与子代理会话镜像；协议级取消；审批与 `ask_user_question` 交互桥 | web |
| `censorscope-ui` | 0.1.1 | 浏览器会话「CensorScope」Tab，展示调用轨迹、工具失败原因及事件详情 | web |

## 一键安装（推荐）

与 CensorGuard 组合运行时，先在受管进程树外以 root 启动 `censorscoped`。DSH 插件只调用
Scope 控制面，由 daemon 加载 eBPF 并采集数据。`censorscope-host` 0.1.2 需要同步更新
`censorscopectl`，使用新增的 `--root-pid-namespace` 参数；daemon 控制协议和目标授权规则
保持原有语义。

插件变更必须 bump 版本并重新打包后才能生效（安装器按版本号刷新），请一律通过脚本安装：

```bash
export DSH_HOME=/path/to/dsh/home     # dsh 配置目录
./scripts/install-censorscope.sh all        # host(web+headless) + agentcensor-session-proxy(web) + ui(web)
```

模式说明：

- `all`：安装/刷新全部三个插件。
- `host`：安装/刷新 `censorscope-host`（web+headless）与 `agentcensor-session-proxy`（web）。

`all` 模式安装 CensorScope 轨迹页，并通过 `censorscope-ui` 的 patch 设置
`ui-trajectory: disabled`。`host` 模式保留已有的 UI 插件配置。

其它要点：

- 脚本幂等：版本未变时输出 `installed` 并跳过；版本变化时自动 `npm pack` 并用 `dsh plugin --profile <p> add` 刷新。
- 自定义包管理器缓存目录：
  ```bash
  export PNPM_STORE_DIR=<writable-dir> PNPM_CACHE_DIR=<writable-dir> NPM_CACHE_DIR=<writable-dir>
  ```
- 装完插件后重启 dsh web 即生效；更新 eBPF 采集或 daemon 时，重新构建 daemon 并重启 censorscoped。
- 手动打包（等价于脚本内动作）：
  ```bash
  cd plugins/<pkg> && npm pack --pack-destination . --cache <工作区 .cache/npm>
  ```
