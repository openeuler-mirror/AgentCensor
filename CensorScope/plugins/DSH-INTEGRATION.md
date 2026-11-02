# dsh 集成设计约束与归因方案

## 必须保留的行为

- **确定的 SessionId 归因。** 对由会话发起的真实操作，需要确定地识别所属 SessionId。
  覆盖范围包括原本由 dsh 根进程直接执行的操作，例如原生文件读写，不限于 shell 子进程。
  多会话并发时不能串号，也不能把只能归属到宿主 PID 的事件当成已完成会话归因。
- **worker 可以被替代。** 每会话常驻 dsh worker 是当前实现手段，不是必须保留的架构。
  替代方案必须提供同等的事件身份关联能力，并说明覆盖范围、遗漏和歧义处理。
- **关闭原生轨迹。** `censorscope-ui/cordis.patch.yml` 中的 `ui-trajectory: disabled`
  是有意设置。使用 CensorScope 轨迹页，不把恢复原生轨迹列为兼容优化目标。
- **不修改 dsh 源码。** 使用外部插件、profile/bundle、可替换执行后端和 CensorScope
  侧实现。其它原生交互能力应尽可能保留。

进程环境中的 `CENSORSCOPE_SESSION_ID` 是当前实现的身份载体，不限定未来必须使用这个
载体。工具日志、应用层事件和 eBPF 观测事件属于不同证据层次；记录了工具的 SessionId，
不等于该工具产生的每条内核事件已有确定的 SessionId。公共后台操作没有唯一会话时，应
保留共享或无会话属性，不能为了填充字段而随意指定会话。

## 版本核查

核查日期：2026-11-02。

- 本地 `~/deepseek-harness` HEAD 为 `b830ec950faba0a46dda564be3e2d854afed94f1`，
  精确对应 `dsh-v0.2.1-rc.5`，以下源码结论以此为基线。
- 官方 [npm 包元数据](https://registry.npmjs.org/@deepseek-ai%2fdsh) 的 `latest` 和
  `next` 都为 `0.2.1-rc.5`，`alpha` 为 `0.2.2-alpha.2`；版本列表没有 `2.1.0` 或
  `0.2.1` 正式版。
- GitHub 最近发布的是 [dsh-v0.2.1-rc.5](https://github.com/deepseek-ai/deepseek-harness/releases/tag/dsh-v0.2.1-rc.5)
  （2026-10-30，预发布版）。另有 2026-10-28 发布的 `0.2.2-alpha.2`，不是本次源码
  验证对象。不要把 `0.2.1` 写成 `2.1.0`，也不要把 npm 的 `latest` 标签理解为正式稳定版。

这是能力与设计核查，不表示现有 CensorScope 插件已通过 rc.5 的端到端兼容验收。

## 原生接口能提供什么

下列源码路径相对于 dsh 源码根目录。

| 接口或组件 | 能力 | 不能据此保证的事情 |
|---|---|---|
| `ToolExecution.agent`、`callId`、`rootCallId`；`tools/execute` | 在工具执行边界取得会话和调用身份，可供外部插件使用 | 不自动标记该调用产生的每条系统调用；非工具操作也不全经过此边界 |
| `agents.currentInitiator()`、`withInitiator()` | 基于 AsyncLocalStorage 继承异步驱动链的 Agent；原生 agent-loop 启动驱动时建立该边界 | 不是通用的当前文件操作所有者；可能没有 initiator，子代理 setup 中可能是因果父 Agent；不向内核传播身份 |
| `FileSystem` / `ctx.fs` | 可通过外部插件替换文件系统提供方；接口覆盖 resolve、stat、文本/二进制读、流式读、目录、原子写和编辑等 | 不是所有插件直接调用 `node:fs` 的统一拦截器，也不自带每会话远程执行后端 |
| `fs/observed`、`fs/write-intent`、`fs/edit-intent` | 部分原生文件工具传递 exec actor，可识别会话并维护读写前置条件 | 不是完整 I/O 审计流；并非每次 stat/open/read/write 都会发出，也不能据此与内核事件逐条对应 |
| `shellEnv` 的原生 `DSH_SESSION_ID` | 给模型 shell 执行传递会话身份 | 无法覆盖 dsh 根进程直接执行的文件读写；其它 subprocess 路径也需逐项核对 |
| `fs-sandbox` | 在本地文件系统提供方上对写和编辑做策略检查 | 仍然委托本地实现执行 I/O，不会自动把操作移到会话进程 |
| PTC runtime、OTel 会话日志 | 分别提供程序化工具执行和带 SessionId 的应用日志 | PTC 的工具回调仍可能在宿主执行；日志中的 SessionId 不等于内核事件的身份关联 |

对应源码：

- `packages/core/tools/src/index.ts`：工具执行上下文及 waterfall。
- `packages/core/agent/src/index.ts`、`packages/core/agent-loop/src/agent.ts`：initiator 边界。
- `packages/fs/fs/src/index.ts`、`packages/fs/fs-local/src/index.ts`、
  `packages/fs/fs-local/src/fsio.ts`：可替换契约与实际本地 Node I/O。
- `packages/fs/fs-sandbox/src/index.ts`、`packages/fs/tool-fs/src/read.ts`、
  `packages/fs/tool-fs/src/write.ts`：沙箱及 observation actor。
- `packages/fs/tool-fs-search/src/index.ts`：glob/grep 通过 subprocess/ripgrep 执行，
  不走 `ctx.fs`，需要单独覆盖。

**结论：有可用于构建替代方案的公开扩展接口，没有开箱即用的通用内核事件会话标签。**
不能仅因为 rc.5 能取得 Agent 上下文，就删除当前 worker 并声称归因要求已经满足。

## 可讨论的替代方案

| 方案 | 对确定归因的作用 | 实现与覆盖边界 |
|---|---|---|
| 保留每会话完整 dsh worker | 让该 worker 内执行的会话操作具备进程级会话身份 | 需修复现有代理的兼容问题；worker 内若同时执行多个子会话，根会话环境变量不能自动区分子会话 |
| 原生 Web agent-loop + 外部文件系统后端 + 每会话 I/O helper 进程 | 把原本在宿主执行的文件 I/O 移到持有确定会话身份的进程，避免代理整个会话 | 利用 `ctx.fs` 替换点；由明确 exec 身份或经验证的 initiator 路由。需覆盖 resolve/stat/流式读/写/编辑等完整契约、取消、沙箱、跨进程并发写锁及文件版本检查。直接 `node:fs`、网络和其它子系统不自动覆盖 |
| 原生 Web + 外部运行时/原生 I/O 标记桥 + Scope 关联 | 保持 I/O 在根进程，通过请求级标记关联到真实内核事件 | 需自行设计并验证 libuv 请求与执行线程、文件描述符和系统调用的关联，覆盖同步/异步/流式路径。只有 AsyncLocalStorage 或线程时间窗不够；不是 dsh 已提供的方案 |
| 仅导出工具/文件 observation 事件 | 可准确记录应用层操作的会话身份 | 不能替代底层内核事件的确定归因；如另行采用，必须明确标注证据来源和覆盖范围 |

值得优先验证的是 **保留原生 Web agent-loop，只外移需要归因的执行能力**。文件系统
helper 可以比完整 dsh worker 更小，减少对会话流、模型切换和浏览器控制的代理需求。
这仍需实现外部插件和执行后端，且必须先证明覆盖范围；当前不据此移除完整 worker。

以下方式不能作为确定归因的替代：在共享进程中反复切换 `process.env`，按 PID/TID 当前
活动会话赋值，或仅凭工具起止时间及路径匹配。Node 异步 I/O 可以并发、交错并复用 libuv
线程；共享后台任务也会产生 I/O。存在多个候选时必须保留歧义，不能选一个会话充数。

## 替代方案验收条件

- 两个以上会话并发执行文件读写、访问同一路径时，内核观测事件不能相互串号。
- 覆盖原生文件工具的真实 I/O，包括路径解析、元数据、临时文件、原子发布及流式读取；
  明确 instructions/skills、附件、持久化和直接 `node:fs` 路径的归属与覆盖边界。
- 子代理和后台任务具有正确的会话身份；父会话环境变量不能代替子会话归因。
- 保留取消、沙箱、原子写、版本检查以及跨会话并发控制；helper 崩溃和重启不得错配身份。
- 应用层记录与 eBPF 记录分别标明来源；无法建立关联时明确报告，不静默降级为时间窗猜测。
- 原生 `ui-trajectory` 保持关闭，CensorScope 轨迹页继续提供观测入口。

## 优化进展与验证范围

- worker 启动修复：使用 `seed` 模式，保留原生用户 skills 目录，重新启用 headless
  默认禁用的账户服务；增加启动超时、错误处理及临时目录清理。
- 工具调用边界：通过 `ToolExecution.agent.session` 获取实际会话身份，在
  `tools/execute` 调用前记录开始时间，正常返回、报错和取消均记录结束；逐调用保持
  start/end 报告顺序，不等待观测服务再执行工具，忽略宿主镜像事件。
- 这些调用 span 是应用层边界，不能修复 worker 中子会话文件 I/O 的进程级归因。
  子会话 shell 还可能继承父会话 `CENSORSCOPE_SESSION_ID`；不能把调用 span 的
  SessionId 修正宣称为子会话所有内核事件已经准确归因。

测试命令和范围见插件 README；仍需按上面的条件完成替代方案及浏览器端到端验收。
所有改动位于 CensorScope，dsh 源码保持不变。
