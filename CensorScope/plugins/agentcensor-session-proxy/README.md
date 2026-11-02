# agentcensor-session-proxy

让 **dsh web** 的每个会话真正跑在独立的 dsh 子进程(worker)里的外部插件。
worker 是完整的 dsh 进程、跑正常的原生 agent 流程,宿主只做 UI 中间人与透明回放。

worker 的首要职责是让原本在 dsh 根进程内执行的会话操作（例如原生文件读写）具有明确的
进程级会话身份，而不只是给 shell 子进程打标签。**确定的 SessionId 归因是硬要求，
worker 本身不是硬要求。** 可以采用其它实现，但必须在并发情况下同样确定地关联真实
观测事件与会话，不能用工具日志或时间窗推测代替。不要据此假定现有实现已覆盖所有子会话。

Web 端的原生 `ui-trajectory` 视图必须关闭，由 `censorscope-ui` 提供 CensorScope 轨迹页。
这是预期架构约束，不是可选的界面偏好。

不修改 dsh 源码。版本核查、替代方案与验收边界见
[dsh 集成设计约束与归因方案](../DSH-INTEGRATION.md)。

## 工作方式

每个会话在发出第一条消息时**懒启动一个常驻 worker 进程**(原生 agent-loop):它共享宿主
的全部配置(`settings.yaml` / `.credentials.yaml` / 机器级 `cordis.patch.yml` / env 分层,
与原生 dsh 读同一文件,不存在任何 key/模型 env 注入);worker 只把**可写数据根**(会话
持久化、storage、attachment)重定向到私有临时目录,共享会话日志的唯一写者仍是宿主 ——
宿主保留 worker 的对话、工具、审批事件内容并写入共享日志。浏览器修改权限、沙箱、审批
或模型时，宿主将原生配置事件同步到常驻 worker，两端各自维护连续的事件序号。

## 要求

- 历史运行要求为 dsh **0.1.5-rc.2**、Node 22；本次源码核查基线为 **0.2.1-rc.5**，
  不代表插件已经通过该版本的完整兼容验收。版本来源见上述设计文档。

## 安装与启动

```sh
# 安装
dsh plugin --profile web add /path/to/agentcensor-session-proxy-0.1.7.tgz
# 启动
dsh --profile web
```

没有全局 `dsh`、直接从源码运行时，在 DeepSeek Harness 根目录执行 `pnpm dsh --profile web`；插件会
自动用同一源码入口启动 headless worker。也可在启动 Web 时设置 `DSH_ROOT`，或用
`AGENTCENSOR_DSH_BIN` 指向独立的 `dsh` 可执行文件。

worker 保留宿主 `DSH_AGENTS_HOME`（未设置时使用 dsh 原生默认目录），并在 headless
配置中重新启用 `credentials`、`authorization`、`deepseek-account`。启动等待上限为
60 秒；启动失败或超时会终止子进程并清理临时目录。

使用本地 dsh 源码执行兼容测试：

```sh
DSH_ROOT=~/deepseek-harness npm test
```

测试在临时目录配置源码解析，不改写 dsh。启动测试使用原生 Context/Session 和测试
子进程验证协议、配置继承与清理，不调用模型，不等同于完整浏览器端到端验收。

## 卸载
```sh
dsh plugin --profile web remove agentcensor-session-proxy
```

## 行为细节

- 每个 web 会话 ↔ **一个常驻 worker 进程**;首条消息才 spawn(懒加载),后续消息经
  stdin 帧直接交给同一进程,不逐条重建、不逐条 resume。
- 输入保留原生 `next-turn` / `next-step` 和 `wakeup`：follow-up 排下一轮、steer
  进入最近一步、inject 只排队不唤醒。worker 收到后立即交给原生 inbox，取消时由
  原生循环处理 `keepInbox`。宿主投影显示已镜像的 pending 消息，编辑/删除按消息 id
  转发给 worker；传输期间若消息已被原生循环领取，编辑/删除可能成为无操作。
- 事件回灌:worker 把会话事件实时打到 stdout(`ACEVT` 行协议),宿主逐条回放进宿主
  会话 → 浏览器 follow 流 / 投影 / JSONL 实时更新(共享日志始终连续、宿主唯一写者)。
- 模型选择:默认选型读宿主 `settings.yaml` 的 `agent-default-model`(worker 每次
  boot 从自身持久化重放推导会话最近一次选型,仅在进程重建时发生)。
- 常驻 worker 的权限、沙箱、审批及模型切换通过 `host-session-event` 同步；worker
  用原生 session fold 应用，发 `ACSYNC` 确认，不把同一配置事件再次写回宿主。
- 模型切换同时更新原生请求选型引用，在下一次 prompt assembly 生效；已组装请求保持原选型。
- 实时文本、推理和工具参数通过独立 `ACSTREAM` 控制帧传回宿主的原生
  `agent/assistant-stream`，供原生 follow/reconnect 消费。worker 替换时关闭未完成流，
  保持宿主 revision 连续并转换提交序号；不启用原生轨迹页。
- goal / subagent 等 worker 内状态会跨消息存活；后台任务不会因单轮结束被杀。
  jobs/schedule 的宿主 API 是独立的进程内注册表，目前没有通过代理暴露完整的
  list/follow/kill 或 schedule 控制面，不能宣称与原生 Web 管理面等价。
- **审批 / 提问（ask_user_question）桥**:浏览器是 dsh 里唯一的交互应答端,但它只挂在**宿主**的
  remote waterfall(`api-remotes` 的 `approval/request` / `user-questions/request`),worker 里既没有
  这条桥也没有客户端,所以原生 ask 会立刻 fail-closed(`unavailable` / `NO_PROVIDER`,日志表现为
  `approval/decided{outcome:"unavailable"}`)。现在 worker 侧注册了这两个 waterfall 的应答器:把
  ask 经 `ACASK` 帧转给宿主,宿主用**自己的 agent 作 subject 派发同一个 waterfall**(控制面,不写
  任何会话数据),浏览器用现成的弹窗作答,答案再经 `answer` 帧回到 worker 由原生服务落地。同时
  worker overlay 补挂 `tool-ask-user`,让模型重新拿到 `ask_user_question`(它原本只存在于 web 面才
  挂载的 `standard` agent preset 里)。没有浏览器/没人回答时,应答器一律 `next()`,行为与不装
  插件时**完全一致**(fail-closed,不会挂住);按停止或工具调用被中止时,worker 发 `ACWITHDRAW`,
  宿主中止那次派发,浏览器弹窗随之消失,工具拿到原生 `cancelled` 结果。
- **停止按钮 = 协议级取消**:浏览器的停止调用 `agent.cancel({kind:'user'},{keepInbox:true})`,
  宿主把这个取消**转发**给 worker(`{"type":"cancel",...}` 帧),由 worker 里持有原生
  agent-loop 的 agent 自己中止当前 turn —— 与不装插件时完全相同的原生路径:已开始的
  tool call 收到中止结果、`step/end`、`turn/end {reason:{kind:'aborted',reason:{kind:'user'}}}`,
  这些事件照常 1:1 回放进宿主会话。worker 进程**不会被杀**、常驻状态保留,下一条消息仍由
  同一个进程服务;因为 turn 是被正常收口的,下一次 resume 也不会再出现崩溃修复
  (`interruptedTurnClosers` + `session/end-seed`)带来的 seq 偏移。

## 已知限制

- 图片/附件消息:消息对象会原样转发,但附件字节与宿主 attachment 库的对接未完成。
- timed `ask_user_question` 的 wait 倒计时字段会转发给浏览器；宿主服务没有把 worker
  的业务 wait 注册到自己的 `UserQuestions.attachWait`，因此需要完整端到端验证，不能把
  普通问题桥当作 timed wait 等价实现。
- worker 内 subagent 的**子会话**持久化日志暂不进宿主(父会话 tool/result 可见)。
- 子会话 worker 内直接执行的根进程文件 I/O 仍可能继承父进程环境；应用层工具 span
  已按 `ToolExecution.agent.session` 区分，但这不等于每条内核事件都完成子会话归因。
- 宿主进程异常退出时,worker 靠 stdin 断开发起优雅退出;极端情况下残留的临时私有
  目录属可清理垃圾(不影响会话数据,宿主 durable 始终完整)。
- worker 进程**崩溃**(非正常结束)时不会走协议取消:宿主会话里那一轮仍是未闭合状态,
  下次 spawn 的 worker resume 会补 `interruptedTurnClosers`(+3)+`session/end-seed`(+1),
  而宿主的回放游标只认 1:1/`+1`,于是会打 `mirror gap` 并停止回放(需要重启 dsh 才能恢复)。

## 目录

```
agentcensor-session-proxy/              插件源码(可读/可改;安装请用下方的 tgz)
├── lib/index.mjs                         宿主侧:会话代理 WebDriverAgent(worker 路由 + 透明回放)
├── lib/worker.mjs                        worker 侧:常驻 runner(原生 agent-loop + 双向帧协议)
├── lib/interaction-bridge.mjs            宿主侧:审批/提问的交互桥(控制面,只派发 waterfall,不写会话数据)
├── package.json / cordis.patch.yml       插件包元数据与 bundle patch
agentcensor-session-proxy-0.1.7.tgz    可安装包(需自行打包)
README.md                              本文件
```

> 源码变更后重新打包(在源码目录执行):`npm pack`(产出 `<name>-<version>.tgz`),
> 再对 profile 重新 `dsh plugin --profile web add <新.tgz>`。
