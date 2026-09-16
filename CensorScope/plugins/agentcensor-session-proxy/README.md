# agentcensor-session-proxy

让 **dsh web** 的每个会话真正跑在独立的 dsh 子进程(worker)里的外部插件。
worker 是完整的 dsh 进程、跑正常的原生 agent 流程,宿主只做 UI 中间人与透明回放。

## 工作方式

每个会话在发出第一条消息时**懒启动一个常驻 worker 进程**(原生 agent-loop):它共享宿主
的全部配置(`settings.yaml` / `.credentials.yaml` / 机器级 `cordis.patch.yml` / env 分层,
与原生 dsh 读同一文件,不存在任何 key/模型 env 注入);worker 只把**可写数据根**(会话
持久化、storage、attachment)重定向到私有临时目录,共享会话日志的唯一写者仍是宿主 ——
宿主对 worker 的事件流做 1:1 透明回放(不合成、不篡改)。

## 要求

- Node 22；dsh **0.1.2-rc.1** 完整支持。dsh **0.1.1-rc.2** 可使用兼容模式，
  仅跳过该版本尚未导出的 `turnBoundary` UI 投影。

## 安装与启动

```sh
# 安装
dsh plugin --profile web add /path/to/agentcensor-session-proxy-0.1.0.tgz
# 启动
dsh web
```

没有全局 `dsh`、直接从源码运行时，在 DeepSeek Harness 根目录执行 `pnpm dsh web`；插件会
自动用同一源码入口启动 headless worker。也可在启动 Web 时设置 `DSH_ROOT`，或用
`AGENTCENSOR_DSH_BIN` 指向独立的 `dsh` 可执行文件。

## 卸载
```sh
dsh plugin --profile web remove agentcensor-session-proxy
```

## 行为细节

- 每个 web 会话 ↔ **一个常驻 worker 进程**;首条消息才 spawn(懒加载),后续消息经
  stdin 帧直接交给同一进程,不逐条重建、不逐条 resume。
- 事件回灌:worker 把会话事件实时打到 stdout(`ACEVT` 行协议),宿主逐条回放进宿主
  会话 → 浏览器 follow 流 / 投影 / JSONL 实时更新(共享日志始终连续、宿主唯一写者)。
- 模型选择:默认选型读宿主 `settings.yaml` 的 `agent-default-model`(worker 每次
  boot 从自身持久化重放推导会话最近一次选型,仅在进程重建时发生)。
- goal / schedule / subagent / jobs 等工具在常驻 worker 内与原生语义一致地跨消息存活
  (后台任务不会因单轮结束被杀)。

## 已知限制(持续演进中)

- 需要用户交互的操作(**审批**与 `ask_user_question` 提问)尚未桥接回浏览器 UI:
  模型发起这类调用时会在 worker 内等待(计划中)。
- 图片/附件消息:消息对象会原样转发,但附件字节与宿主 attachment 库的对接未完成。
- worker 内 subagent 的**子会话**持久化日志暂不进宿主(父会话 tool/result 可见)。
- 宿主进程异常退出时,worker 靠 stdin 断开发起优雅退出;极端情况下残留的临时私有
  目录属可清理垃圾(不影响会话数据,宿主 durable 始终完整)。

## 目录

```
agentcensor-session-proxy/              插件源码(可读/可改;安装请用下方的 tgz)
├── lib/index.mjs                         宿主侧:会话代理 WebDriverAgent(worker 路由 + 透明回放)
├── lib/worker.mjs                        worker 侧:常驻 runner(原生 agent-loop + 双向帧协议)
├── package.json / cordis.patch.yml       插件包元数据与 bundle patch
agentcensor-session-proxy-0.1.0.tgz    可安装包(安装即用)
README.md                              本文件
```

> 改版后重新打包(在源码目录执行):`npm pack`(产出 `<name>-<version>.tgz`),
> 再对 profile 重新 `dsh plugin --profile web add <新.tgz>`。
