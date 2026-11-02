# CensorScope × dsh 插件

本目录包含三个随 dsh 预装的插件（不修改 dsh 源码，只做 profile 级插件安装）：

## 设计约束

- **SessionId 确定归因是硬要求，worker 不是硬要求。** 需要覆盖原本在 dsh 根进程内
  执行的会话操作（包括原生文件读写），不能只覆盖 shell 子进程。每会话 worker 是当前
  实现方式；其它方案只要能在并发情况下确定地关联真实观测事件与会话，也可替代它。
- **原生 dsh 轨迹视图必须关闭。** `censorscope-ui` 明确禁用 `ui-trajectory`，CensorScope
  轨迹页是预期的轨迹入口；这不是原生体验兼容缺陷。
- **不修改 dsh 源码。** 适配和替代方案通过外部插件、profile/bundle、执行后端或
  CensorScope 实现，不以修改 dsh 为前提。

版本核查、原生接口、替代方案及验收边界见 [dsh 集成设计约束与归因方案](DSH-INTEGRATION.md)。

| 包 | 版本 | 作用 | 装入 profile |
|---|---|---|---|
| `censorscope-host` | 0.1.2 | main 角色：单 trace track-add(trace-id 复用；显式传递自身 PID namespace，兼容 Guard 的 ptrace 限制) + per-call cache writer + `/censorscope/call` 路由；worker 角色：注入 `DSH_CENSORSCOPE_CALL_ID` + call-start/end span（真实起止时刻） | web + headless |
| `agentcensor-session-proxy` | 0.1.1 | web 每会话 headless worker（agentcensor 拓扑）+ `CENSORSCOPE_SESSION_ID`；停止按钮 = 协议级取消（worker 内原生 `agent.cancel`）；审批 / `ask_user_question` 交互桥（worker→host→浏览器→回注，控制面不写会话数据） | web |
| `censorscope-ui` | 0.1.0 | 浏览器会话「CensorScope」Tab（替换原生轨迹视图） | web |

## 一键安装（推荐）

与 CensorGuard 组合运行时，先在受管进程树外以 root 启动 `censorscoped`。DSH 插件只调用
Scope 控制面，不在 Web/worker 中加载 eBPF。`censorscope-host` 0.1.2 需要同步更新
`censorscopectl`，使用新增的 `--root-pid-namespace` 参数；daemon 控制协议和目标授权规则
保持原有语义。

插件变更必须 bump 版本并重新打包后才能生效（安装器按版本号刷新），请一律通过脚本安装：

```bash
export DSH_HOME=/path/to/dsh/home     # 必填，脚本本身不设置
./scripts/install-censorscope.sh all        # host(web+headless) + agentcensor-session-proxy(web) + ui(web)
```

模式说明：

- `all`：安装/刷新全部三个插件。
- `host`：只装 `censorscope-host`（web+headless）与 `agentcensor-session-proxy`（web），不装 `censorscope-ui`。

`host` 模式本身不保证关闭原生轨迹，也不卸载已有 UI 插件。完整集成应使用 `all`，并确认
最终 profile 中 `ui-trajectory` 为禁用状态；其它 bundle 或 profile patch 不应重新启用它。

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
