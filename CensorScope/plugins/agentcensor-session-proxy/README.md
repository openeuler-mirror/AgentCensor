# agentcensor-session-proxy

为 **dsh web** 的每个会话提供独立的常驻 headless worker，执行原生 agent 流程。
worker 通过 `CENSORSCOPE_SESSION_ID` 携带会话身份，供 CensorScope 关联该进程内执行的
文件读写与子进程操作。宿主接收会话事件，负责共享日志写入和浏览器展示。

Web 端由 `censorscope-ui` 提供 CensorScope 轨迹页，通过 profile patch 替换原生轨迹入口。

## 运行环境

- Node.js 22。
- 插件使用 dsh 0.2.1 系列的 Session V3 事件、Agent 初始化事务和会话持久化读取接口。

## 安装与启动

```sh
# 安装
dsh plugin --profile web add /path/to/agentcensor-session-proxy-0.1.4.tgz
# 启动
dsh --profile web
```

从源码运行时，在 DeepSeek Harness 根目录执行 `pnpm dsh --profile web`；插件会
使用同一源码入口启动 headless worker。启动 Web 时也可设置 `DSH_ROOT`，或用
`AGENTCENSOR_DSH_BIN` 指向独立的 `dsh` 可执行文件。

## 会话执行与同步

- **常驻执行**：首条消息触发 worker 启动，后续消息通过 stdin 帧交给同一进程。
  goal、schedule、subagent 和 jobs 等任务在该进程内跨消息持续运行。
- **会话初始化**：宿主通过持久化读取接口取得会话事件，分批发送给 worker。
  worker 用这些事件恢复会话，并在发布前完成 Agent 初始化事务。
- **配置共享**：worker 使用宿主的 `settings.yaml`、`.credentials.yaml`、机器级
  `cordis.patch.yml` 和环境配置。会话持久化、storage 和 attachment 使用每个 worker
  的私有临时目录，共享会话日志由宿主统一写入。
- **配置同步**：浏览器中的权限、沙箱、审批和模型配置事件通过 `host-session-event`
  同步到 worker，由原生 session fold 应用，再通过 `ACSYNC` 确认。
- **模型选择**：worker 从会话事件恢复最近的模型选择；新会话使用默认模型配置。
- **事件回放**：worker 通过 `ACEVT` 帧发送会话事件，宿主将其写入共享日志并更新
  浏览器 follow 流和投影。事件保留 `surfaceOp`、`sourceEventSeqs` 等字段，
  `plugin:*` 记录通过插件记录接口写入。
- **子代理会话**：worker 发送子会话事件及工作目录、层级等元数据，宿主创建对应会话，
  供浏览器从会话树查看子代理对话与工具结果。

## 审批与提问

worker 的审批和 `ask_user_question` 请求通过 `ACASK` 帧交给宿主，再由宿主调用
浏览器交互服务。浏览器的回答通过 `answer` 帧返回 worker，由原生服务记录结果。
worker 的 overlay 加载 `tool-ask-user`，为模型提供提问工具。

浏览器连接及回答决定交互结果；交互服务缺席时沿用原生的 `unavailable` 或
`NO_PROVIDER` 结果。停止操作或工具中止时，worker 通过 `ACWITHDRAW` 撤回请求，
宿主关闭对应弹窗，并向工具返回原生 `cancelled` 结果。

## 停止与恢复

浏览器停止操作通过协议转发到 worker 的 `agent.cancel`，由原生 agent-loop 结束当前
turn。工具中止结果、`step/end` 和 `turn/end` 事件回放到宿主，worker 保留常驻状态，
继续服务后续消息。

宿主退出时先发送 shutdown 请求，再按超时策略终止 worker。宿主持有共享持久化日志，
worker 重建时从该日志恢复会话。worker 的附件字节保存在各自的私有目录中，
跨进程附件访问仍需配套的存储对接。

事件序号出现缺口时，宿主记录 `mirror gap` 并从当前事件继续回放；扩展事件根据
宿主支持的记录类型处理，并输出跳过和失败计数。worker 启动连续失败时触发熔断，
日志保留失败信息供排查。

## 卸载

```sh
dsh plugin --profile web remove agentcensor-session-proxy
```

## 目录与打包

```text
agentcensor-session-proxy/
├── lib/index.mjs                 宿主侧会话代理、worker 路由与事件回放
├── lib/worker.mjs                原生 agent-loop 与双向帧协议
├── lib/interaction-bridge.mjs    审批与提问交互桥
├── package.json                 插件包元数据
├── cordis.patch.yml             profile patch
└── README.md                    使用说明
```

源码更新后，在插件目录执行 `npm pack` 生成安装包，再通过
`dsh plugin --profile web add <新.tgz>` 刷新安装。
