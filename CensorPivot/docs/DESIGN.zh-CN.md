# CensorPivot 编排与接入层设计

> 本文侧重系统结构、协议和状态机。部署准备、Guard domain 生效点、`VariantOpen`、
> `censorfs-mounter` 以及单批次逐步执行过程见 [完整运行流程](flow.md)。

## 1. 定位

CensorPivot 位于 Agent Runtime 与工具进程之间，只负责两件事：

1. **接入**：暴露单一 Unix Socket，工具批次必须由它启动。调用进程不能获得
   CensorFS 私有 View、mounter 权限或 CensorGuard launcher 权限，因此正常部署下无法
   绕过这条执行路径。
2. **编排**：把多个工具调用放进同一个文件系统事务，执行 Prepare，持久化唯一决策，
   再反复推进 Commit 或 Abort，直到终态。

CensorPivot 不替代三个数据面：文件版本仍由 CensorFS 保证，系统调用仍由
CensorGuard/eBPF LSM 强制，内核与应用行为仍由 CensorScope 采集。

## 2. 系统框架

### 2.1 分层总览

下图严格按职责分层，使用纯文本绘制，不依赖 Mermaid 或 Markdown 图形插件。纵向箭头
表示请求从接入、编排、IPC、组件服务进入内核数据面的主方向；`I0` 至 `I5` 与 2.3 节
接口矩阵逐项对应。CensorPivot 是唯一事务编排者，但文件、安全和观测仍由各自的数据面
完成。

```text
+==================================================================================================+
| L1  CALLERS / 调用方                                                                             |
|                                                                                                  |
|     Agent Runtime / Harness / SDK                                  censorpivot CLI                |
+==================================================+===============================================+
                                                   |
                                      [I0] BatchRequest / JSON
                                                   v
+--------------------------------------------------------------------------------------------------+
| L2  ENTRY IPC / 入口通信                                                                         |
|                                                                                                  |
|     /run/censorpivot/control.sock  |  Unix Stream  |  0660 + SO_PEERCRED                         |
+--------------------------------------------------+-----------------------------------------------+
                                                   |
                                                   v
+--------------------------------------------------------------------------------------------------+
| L3  CENSORPIVOT APPLICATION / 接入与编排                                                         |
|                                                                                                  |
|     Gateway -> Validation -> Idempotency -> Transaction Coordinator <-> Recovery Worker          |
|                                            |                                                     |
|                  +-------------------------+------------------------+----------------------+      |
|                  |                         |                        |                      |      |
|           CensorFS Adapter          Guard Launch Config     CensorScope Adapter     Runner Supervisor|
+------------------+-------------------------+------------------------+----------------------+------+
                   |                         |                        |                      |
                   v                         v                        v                      v
+--------------------------------------------------------------------------------------------------+
| L4  LOCAL IPC & INTERFACES / 下游本机接口                                                        |
|                                                                                                  |
| [I1] CensorFS control UDS     [I3] Guard launch UDS       [I4] Scope control UDS   [I5] pipes     |
|      Protobuf frames               JSON Lines v3               length#value             JSONL     |
| [I2] AttachFuse + SCM_RIGHTS                                                            request/ |
|      on same CensorFS UDS          JSON Lines v3                                         reply    |
+------------------+-------------------------+------------------------+----------------------+------+
                   |                         |                        |                      |
                   v                         v                        v                      v
+--------------------------------------------------------------------------------------------------+
| L5  COMPONENT SERVICES & CONTROLLED EXECUTION / 组件服务与受控执行                               |
|                                                                                                  |
| CensorFS                     CensorGuard                  CensorScope               Pivot worker  |
|   censorfs CLI                 censorguardd                 censorscopectl             supervisor  |
|   censorfsd                    censorguard-exec             censorscoped                  |        |
|   censorfs-mounter                   |                                                    |        |
|          |                            +-------------------+--------------------------------+        |
|          +----> [Mount Namespace + writable View]        |                                         |
|                                                          v                                         |
|     Controlled process chain: censorfs-mounter -> censorguard-exec -> Batch Runner -> Tool 1..N  |
+--------------------------------------------------------------------------------------------------+
                                                   |
                                                   v
+--------------------------------------------------------------------------------------------------+
| L6  LINUX KERNEL DATA PLANE / 内核数据面                                                         |
|                                                                                                  |
|   VFS + FUSE (/workspace)    eBPF LSM (file/exec/net)    tracepoint/uprobe/procfs    Network Stack|
|          |                            |                            |                       |        |
|          +--> CensorFS               +--> CensorGuard             +--> CensorScope        +--> WAN |
+--------------------------------------------------------------------------------------------------+
                                                   |
                                                   v
+--------------------------------------------------------------------------------------------------+
| L7  PERSISTENCE & RUNTIME STATE / 持久化与运行时状态                                              |
|                                                                                                  |
| Pivot Transaction JSON       CensorFS store              Guard policy/BPF maps       Scope SQLite|
| durable Commit/Abort         Journal/Object/Manifest     Policy/Audit                Trace/Event |
+==================================================================================================+
```

### 2.2 分层职责

| 层级 | 核心职责 | 明确不负责 |
|---|---|---|
| 第 1 层：调用方 | 生成工具批次，提供 session、trace、策略组和预期 Branch Head | 不直接获得 View、mounter 或 Guard launcher 权限 |
| 第 2 层：入口 IPC | 通过 Pivot UDS 承载一个完整请求/响应，以 socket 权限形成第一道本机边界 | 不解析三个下游组件的协议；初版不开放 TCP 入口 |
| 第 3 层：Pivot 应用 | 识别真实 UID、校验输入和幂等键、管理批次状态机、持久化决策并恢复第二阶段 | 不翻转既有决策，不实现文件/安全/观测数据面 |
| 第 4 层：下游 IPC | 固定 Pivot 到三个组件及 runner 使用的 socket、管道、帧格式和身份来源 | 不包含业务状态机，不承载工具的外联网络流量 |
| 第 5 层：组件服务与执行 | 把事务映射为三个组件调用；在一次挂载和一棵受保护进程树中执行工具 | 不复制三个组件的数据面实现 |
| 第 6 层：Linux 内核数据面 | FUSE 文件 I/O、eBPF LSM 强制、tracepoint/uprobe/procfs 观测和真实网络栈 | 不决定事务 Commit/Abort |
| 第 7 层：持久化与状态 | 保存 Pivot 决策、CensorFS 版本、Guard 策略/审计和 Scope 事件 | 各存储之间不存在共享数据库或隐式事务 |

### 2.3 本机 IPC 与接口矩阵

初版的控制面全部是**本机通信**。Pivot 不监听 TCP，也不调用 CensorGuard 的
`127.0.0.1:50051` gRPC 委托接口；该 gRPC 接口属于 DSH/UI 集成，不在 Pivot 的可信
执行链上。工具自身访问外部网络属于内核数据面，受 CensorGuard 强制并由 CensorScope
观测，不是 Pivot 组件间 RPC。

| ID | 调用方 -> 被调用方 | 端点与传输 | 编码/协议 | 使用的接口 | 身份与失败语义 |
|---|---|---|---|---|---|
| I0 | Agent/CLI -> CensorPivot | `/run/censorpivot/control.sock`，Unix Stream | 每连接一个 JSON 请求和一个 JSON 响应，写半边关闭定界 | `execute`、`status`、`recover`、`doctor` | `0660` + `SO_PEERCRED`；请求不能伪造 UID |
| I1 | Pivot FS Adapter -> `censorfs` -> `censorfsd` | 子进程 + `/run/censorfs/control.sock` | 4-byte little-endian 长度 + Protobuf `ControlRequest/Response` | `VariantOpen`、`VariantPrepare`、`VariantPublish`、`VariantAbort` | daemon 取 socket peer UID/GID；持久 request UUID 保证幂等重试 |
| I2 | `censorfs-mounter` -> `censorfsd` | `/run/censorfs/control.sock` | 同一 Protobuf 帧，并用 `SCM_RIGHTS` 传 `/dev/fuse` FD | `AttachFuse` | root-only mounter；一个 writable View 只 attach 一次 |
| I3 | `censorguard-exec` -> `censorguardd` | `/run/censorguard/launch.sock`，Unix Stream | JSON Lines protocol v3 | `RegisterSelf` | daemon 用 `SO_PEERCRED` 取得真实 PID，写入 domain/group 和 PID 内核映射；成功后才 exec runner；`--failure-policy deny` |
| I4 | Pivot Scope Adapter -> `censorscopectl` -> `censorscoped` | 子进程 + `/run/censorscope/censorscoped.sock` | `length#value` 字段帧 | `TrackAdd`、`CallStart`、`CallEnd`、`TrackRemove` | runner 就绪后以真实 PID 建 Trace；required 模式失败即 Abort，否则写 `warnings` |
| I5 | Pivot Runner Supervisor <-> Batch Runner | 父子进程匿名 stdin/stdout pipe | 每行一个 `RunnerRequest/RunnerReply` JSON | `ToolCall -> CallResult` | FD 只由受控子进程继承；EOF 结束批次，不对系统其他用户开放 socket |

### 2.4 三条运行期数据路径

| 数据路径 | 分层流向 | 最终结果 |
|---|---|---|
| 文件事务路径 | Tool syscall -> VFS/FUSE `/workspace` -> `censorfsd` -> Ticket/View -> Candidate -> Generation/Branch CAS | Abort 丢弃私有修改；Commit 原子推进 Branch Head |
| 安全强制路径 | Tool syscall -> eBPF LSM hook -> scope/policy BPF maps -> allow 或 `-EPERM` -> audit event | 插件、SDK 或子进程无法绕过内核检查 |
| 观测路径 | Tool process/env -> tracepoint/uprobe/procfs -> `censorscoped` -> SQLite read views/export | 事件归属到 session、trace 和 call，不参与放行或提交决策 |
| 外联网络路径 | Tool `connect/send/recv` -> CensorGuard network hook -> host network stack；同时由 CensorScope L2/L3 collector 观测 | Guard 决定能否外联；Pivot 不代理业务流量，也不能回滚已发生的远端副作用 |

### 2.5 一次批次的主路径

```text
BatchRequest
  -> 接入校验
  -> CensorFS VariantOpen
  -> mounter -> Guard launcher -> register_self/domain 生效 -> exec runner
  -> 常驻 runner 就绪并回报 PID
  -> CensorScope TrackAdd(runner PID)
  -> Tool 1..N，每步执行 CallStart -> Tool -> CallEnd
  -> runner 退出 -> CensorScope TrackRemove
  -> CensorFS VariantPrepare
  -> 决策原子落盘
  -> Commit: VariantPublish(CAS) | Abort: VariantAbort
  -> CensorScope 持续记录每个 call 与底层事件
```

## 3. 接入协议

服务端监听 `socket_path`，每条连接只收一个 JSON 请求，客户端写完后 shutdown 写半边。
Socket 权限默认 `0660`，服务端通过 `SO_PEERCRED` 取得真实 UID；请求体不能声明或覆盖
身份。事务查询仅允许 owner UID 或 root。

请求有四类：

- `execute`：提交完整 `BatchRequest`，同步推进到终态或“已决定、待恢复”。
- `status`：按 transaction UUID 查询。
- `recover`：重试当前 UID 可见事务的第二阶段。
- `doctor`：检查命令/socket、FUSE/cgroup 条件，并调用 FS `info`、Scope `doctor`。

`request_id` 是业务幂等键。服务端持久化请求的 BLAKE3 指纹，不持久化调用环境变量；
同 ID 同指纹返回已有事务，同 ID 不同指纹返回 conflict。这既避免重复执行，也避免把
token 等环境秘密写入协调日志。

输入边界：最多 64 个调用（可配置）、程序必须是绝对路径、cwd 不得离开
`/workspace`、保留的 Censor* 环境变量禁止覆盖、协议帧和 stdout/stderr 均有上限。
请求组必须位于管理员配置的 `censorguard.allowed_groups`；空组默认不接受。
请求中的 `guard_scope` 保留为兼容字段，实际 domain 使用 `pivot-<transaction UUID>`，
并持久化到返回记录的 `summary.guard_scope`，防止不同批次复用域名导致策略重绑。

## 4. 两阶段状态机

```text
Received -> Preparing -> Prepared -> CommitDecided -> Committed
                 |                         `-> CommitDecided (retry)
                 |
                 `---------> AbortDecided -> Aborted
                                           `-> AbortDecided (retry)
```

### Phase 1: Prepare

1. 原子写入 `Received`，随后写入 `Preparing`。
2. `VariantOpen` 创建 Tx/Ticket/View；其 request UUID 已在第 1 步持久化，可幂等重放。
3. 启动一次常驻 batch runner。`censorguard-exec` 先以 `register_self` 将真实 PID 绑定到
   当前策略 revision 的 baseline + `guard_group`，domain 内核映射成功后才 exec runner。
4. 所有调用在同一个 View 中顺序执行。每次调用前后写
   CensorScope span，执行环境注入 `CENSORSCOPE_SESSION_ID` 与
   `DSH_CENSORSCOPE_CALL_ID`。
5. 全部 exit 0 后执行 `VariantPrepare`，得到不可变 Candidate，写入 `Prepared`。

工具进程路径固定为 `mounter -> censorguard-exec -> __batch-runner -> program`。一个批次
只 attach 一次 View；常驻 batch runner 通过 stdin/stdout 协议顺序执行所有工具。mounter
提供真实 `/workspace` View；Guard launcher 用 SO_PEERCRED 注册 runner，fork/exec 继承使
全部工具子进程不能通过绕开 SDK 逃避内核强制。

Pivot 必须传入 mounter 已有的 `--cgroup-root/--cgroup-state-dir/--cgroup-scope` 参数。
mounter supervisor 在 worker 退出后清理整个 cgroup，Pivot 等待 supervisor 成功退出后才
Prepare；`setsid` 后代仍属于该 cgroup。超时/Drop 优先发送 SIGTERM，让 supervisor 执行清理，
6 秒仍未退出才强制终止。supervisor 自身被 SIGKILL 的残留由 mounter 下次启动回收。

组件 CLI、runner 握手/回复与工具输出统一使用安全 Rust 非阻塞管道，不创建输出读取线程。
每轮读取有预算，输出有上限，等待有超时；超时不能被无限输出或后台进程持有 FD 绕过。

### Decision 与 Phase 2

`Prepared` 后协调器选择 Commit；此前任一失败选择 Abort。`decision` 与
`CommitDecided/AbortDecided` 在同一 JSON 快照内，通过临时文件、`fsync(file)`、
`rename`、`fsync(directory)` 原子持久化。只有该写入成功后才调用 Publish/Abort。

不变量：

- `decision=commit` 的记录只允许调用 Publish，永不调用 Abort。
- `decision=abort` 的记录只允许调用 Abort，永不调用 Publish。
- CensorFS Publish 始终携带初始 `expected_generation + expected_head_seq`，并发 Head
  变化表现为可见的 CommitDecided/CAS 冲突，不会静默覆盖。
- 每个 CensorFS 阶段都有独立、持久化的 request UUID，网络超时后可取回原结果。

## 5. 崩溃恢复

服务启动时独占 `.coordinator.lock`，扫描全部事务：

| 崩溃时状态 | 恢复动作 | 原因 |
|---|---|---|
| `Received` | 决定 Abort，直接终止 | 尚未接触数据面 |
| `Preparing` | 决定 Abort，必要时重放 Open 取得 Ticket，再 Abort | 不重放可能有外部副作用的工具 |
| `Prepared` | 决定 Commit，再 Publish | Candidate 证明全部工具成功且文件已冻结 |
| `CommitDecided` | 只重试 Publish | 决策不可翻转 |
| `AbortDecided` | 只重试 Abort | 决策不可翻转 |
| 终态 | 不操作 | 幂等返回 |

最关键的取舍是：工具执行不是通用可重放操作。若进程在一次调用完成与结果落盘之间
崩溃，CensorPivot 不会猜测或再次执行，而是 Abort 整批。这样可能损失一次可提交结果，
但不会因恢复机制主动制造第二次外部副作用。

## 6. 三组件契约

### CensorFS

使用现有 `variant-open / variant-prepare / variant-publish / variant-abort` CLI JSON 接口。
一个批次只创建一个 writable View。Publish 的 Branch Head CAS 是最终并发控制点。

### CensorGuard

Pivot 不调用 `evaluate_intent`，也不依赖 Guard control socket。安全边界是每个工具都必须
经 `censorguard-exec --failure-policy deny` 启动：launcher 在 exec runner 前调用
`register_self`，daemon 用 `SO_PEERCRED` 取得真实 PID，解析当前 revision 的 baseline +
`guard_group`，写入 domain/group 和 PID 的内核映射。任一步失败都不 exec runner。

### CensorScope

每个工具调用上报显式 call span，同时将归因环境传进工具树。`required=false` 是默认值：
观测故障被降级，但 Guard 和 CensorFS 语义不变；高合规部署可设为 true，此时 span
失败会使事务 Abort。每次工具执行时 batch runner 设置两项归因变量，无需 sudo 保留
调用方环境。非 required 的观测错误会进入事务 `warnings`，不会被静默吞掉。

## 7. 一致性边界

CensorPivot 提供的是“文件提交 + 协调决策”的原子结果，不是任意分布式副作用事务。
网络 API、外部数据库、邮件、队列和设备 I/O 一旦由工具发出，Abort 无法撤销。
因此生产策略应将 Prepare 阶段限制为：

- 对 `/workspace` 的读写；
- 可重复读取；
- 有业务幂等键的外部操作；
- 或由 CensorGuard 明确拒绝的外部写操作。

需要真正跨系统提交时，应为该系统增加具备 `prepare/commit/abort` 契约的参与者，而不是
把普通 shell 命令宣称为分布式事务。

## 8. 初版限制与演进

- 当前 daemon 串行处理请求，保证单进程状态简单；后续可按 branch 分片锁并并发执行。
- 日志为每事务 JSON 快照，适合初版审计和恢复；高吞吐版可迁移到 SQLite WAL，但必须
  保留 decision-first 与 fsync 语义。
- `doctor` 已检查 FS/Scope 往返及 FUSE/cgroup 条件；Guard hook health 与真实挂载仍需全栈验收。
- 暂无取消 API。取消也必须转换为 durable Abort，不能直接 kill 后删除记录。
- 初版不自动安装 systemd/sudoers，避免在不清楚部署 UID/组的情况下扩大权限。

## 9. 安全部署要求

- Pivot daemon 必须使用专用非 root UID；只有 mounter 通过精确 sudo 规则短暂获得特权，
  工具最终降权到 CensorFS View owner（即 Pivot 专用 UID），绝不以 root 执行。
- Agent 用户只能连接 Pivot socket，不能直接执行 mounter、访问 CensorFS control socket、
  修改事务目录或直接使用 CensorGuard launch socket。
- Pivot binary/config/state 由 root 管理；`state_dir` 不放在 NFS/CIFS 等弱 rename/fsync
  语义文件系统上。
- mounter 使用固定绝对路径和精确 sudoers argv 能力，禁止通配命令与任意环境保留。
- Guard 策略必须同时限制文件、exec、network，并监控 eBPF LSM hook 健康状态。
- 对事务日志设置容量、保留和敏感输出处理策略。初版会持久化截断后的 stdout/stderr，
  不会持久化 env 值。
