# 状态与待办(agentcensor-session-proxy)

> 独立于 README 的开发状态记录,供后续会话/用户续接。最后更新:2026-09-04。

## 1. 最终交付物

```
README.md                                 唯一使用说明(安装/启动/行为/限制)
agentcensor-session-proxy/                插件源码
├── lib/index.mjs                          宿主侧:会话代理(WebDriverAgent;worker 路由 + 透明回放)
├── lib/worker.mjs                         worker 侧:常驻 runner(原生 agent-loop + 双向帧协议)
├── package.json / cordis.patch.yml        包元数据与 bundle patch(禁用 agent-loop + session-title-llm,插入本 bundle)
agentcensor-session-proxy-0.1.0.tgz        安装即用包(与源码一致)
```

安装/启动:

```sh
dsh plugin --profile web add /绝对路径/to/agentcensor-session-proxy-0.1.0.tgz
dsh web
```

约定:代码/配置零绝对路径;插件依赖本机 dsh 0.1.2-rc.1;真实模型 key 与原生 dsh 一致
(env `DEEPSEEK_API_KEY` 或 web Settings/Models 页写 `$DSH_HOME/.credentials.yaml`)。

## 2. 已完成(均已实测)

- **每 session 一个常驻 worker 进程**:首条消息懒 spawn(mode=create/resume、seed 拷贝、
  overlay 钉死私有可写根),后续消息查表直接转交同一进程(原生 followup),turn 结束不销毁;
  会话关闭/宿主退出才优雅停(`shutdown` 帧 / stdin EOF)。
- **双向行协议**:worker→host `ACEVT`(会话事件)/`ACREADY`/`ACIDLE`/`ACERR`;
  host→worker `{"type":"message",…}` / `{"type":"shutdown"}`。
- **共享宿主配置(零注入)**:worker `DSH_HOME`=宿主 home,`settings.yaml`/`.credentials.yaml`/
  机器级 `cordis.patch.yml`/env 分层直读,无 key/模型 env 注入(离线 spike + 真实 E2E 验证)。
- **宿主透明回放**:worker 为内容权威;宿主对其事件 1:1 append 回放,不合成用户可见回复;
  共享 JSONL 唯一写者仍是宿主 —— 依据 rc.1 事实:浏览器 live 流只认宿主 ctx
  `session/event`,而宿主任何 live append 都会被持久化协调器无条件落盘,故"宿主零写入"
  在 rc.1 不可行(不改 dsh 源码前提下),回退态成立。
- **无逐 turn 超时/进程级 turn 控制**;worker 异常按"已回放事件 + 下次消息懒重建 resume"处理;
  boot 以 `ACREADY` 握手(失败即报)。
- 命名统一为 `agentcensor`(无 `m1d`/`dsh-web-worker-bridge` 残留)。
- 已验证:离线冒烟(EOF/shutdown 干净退出、空闲驻留不退出);真实 LLM 闭环(改名前后各一次
  PASS:同会话两条消息 **spawn=1**、store/durable 有序无重复、`worker closed code=0`、stderr 空);
  tarball 安装(`file:` 复制进 profile)在全新 home 可装、`--dump-config` 行正确。

## 3. 剩余问题

### 3.1 功能缺口(原 Phase3 决策项,A/B/C 编号沿用决策记录)

| 编号 | 问题 | 现状 |
|---|---|---|
| B4 | 审批/提问桥:worker 内 `approval` / `ask_user_question` 无应答端,会一直等待 | 未实现(用户已拍板:做完整桥 worker→host→浏览器→回注) |
| A2 | cancel 语义:目前取消 = SIGTERM + 标记,下次消息重建 resume | 协议取消(worker 内原生 `agent.cancel` → 原生 canceled 回合回放)未做 |
| A3 | 镜像非纯回放:仍有 seq 守卫/skip/宿主补插 end-seed/缺口拒绝 | 目标:append-only 1:1 全字段回放,一致性改为"启动前 fail-fast",宿主不发明事件 |
| A4 | boot 失败仍由宿主合成"worker unavailable"回合(仅在 worker 从未 ready 时) | 目标:失败以 worker 自身事件表达,宿主零合成(可保留非 durable 上报) |
| C7 | 事件序列化只取 {type,seq,data,surfaceOp,sourceEventSeqs} 五个字段 | 目标:session.append 全字段透传(dsh 更新加字段不丢) |
| C1 | 图片/附件:消息对象已全量转发,附件字节与宿主 attachment 库未对接 | 未实现(需撤私有 attachment 根重定向等) |
| C3 | `DSH_AGENTS_HOME` 仍覆盖为私有目录 → worker 看不到宿主 `~/.agents` 技能 | 未对齐(撤覆盖,与宿主同根,需评估并发) |
| C2 | worker overlay 仍禁用 `plugin-package-inventory-deepseek` → 请求载荷与宿主差一个扩展 | 待回归:恢复启用并验证 REQUEST_EXTENSION 是否复现 |
| C4 | spawn cwd/.env/沙箱根 vs 宿主启动目录:未核对工具实际取工作目录的来源 | 未对齐(需源码确认 session meta vs process.cwd) |

### 3.2 生命周期健壮性(原 N1-N5,未打磨)

- 会话删除时停止对应 worker、宿主硬退出的子进程清理:目前仅 dispose/进程 `exit` 钩子/
  stdin EOF 兜底,未系统化;
- worker 僵死(非崩溃)无心跳/监督,只能靠协议 cancel 或下次消息懒重建;
- worker 空闲期自主事件(goal/schedule/timer 触发)经长活 stdout 理论上实时可回灌,未单独验证;
- 多会话并发、取消、超长任务在"常驻"模型下未回归。

### 3.3 验证缺口

- 浏览器人工逐字上屏 / 多会话 UI 目视(长期遗留);
- 改名后的"双消息单进程"E2E 未重跑(代码等价,建议补一次);
- check-restart-flow 类"宿主重启后续聊"验收未在新代码上重跑。

## 4. 建议的下一步顺序

1. 回归:双消息单进程、取消、多会话、宿主重启(补足 3.3);
2. A2 协议取消(worker 内原生取消 → 事件回放);
3. A3/A4/C7 纯回放改造(去守卫/去合成/全字段透传,启动前 fail-fast);
4. B4 审批/提问桥;
5. 其余小项:C3/C2/C1/C4 对齐;生命周期 N1-N5 打磨。
