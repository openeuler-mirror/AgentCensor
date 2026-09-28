# AgentCensor 三组件功能与接口说明

## 1. 调查口径

本文只把当前仓库内已有的协议定义、CLI 参数和服务端分发代码视为“已实现
接口”。README 中的路线图不算已实现能力。调查时按以下三层交叉核对：

1. 线上数据结构与操作枚举；
2. 客户端/CLI 是否能构造请求；
3. daemon 是否有对应分发与执行逻辑。

## 2. CensorFS

### 2.1 已有功能

| 能力 | 当前实现 |
|---|---|
| 版本与分支 | 不可变 Generation、Manifest、Branch Head 与 `head_seq` CAS |
| 私有探索 | Ticket + writable View，多 View 的内容和 inode 隔离 |
| 发布与放弃 | `Prepare -> Candidate -> Publish`，以及 Ticket/Candidate/Tx Abort |
| 合并与回滚 | 三方 Merge、冲突报告、以新 Generation 表示回滚 |
| 恢复与幂等 | Journal、持久 request ID、Receipt、启动恢复和离线 fsck |
| 真实文件数据面 | FUSE、独立 Mount Namespace、`/workspace`、mounter 降权执行 |

已知边界包括不支持 symlink、hardlink、xattr、ACL、设备节点、FIFO、socket、
chown、共享可写 mmap、在线 GC 和跨分支原子提交。详细清单见
[CensorFS README](../../CensorFs/README.md)。

### 2.2 已有实现接口

| Pivot 需求 | 已有接口 | 线路与语义 | 代码依据 |
|---|---|---|---|
| 读取分支基准 | `GET_BRANCH_HEAD` / `censorfs --json head` | 返回 `generation_id + head_seq` | [proto](../../CensorFs/api/censorfs.proto)、[CLI](../../CensorFs/cmd/censorfs/src/main.rs) |
| 打开一次私有世界 | `VARIANT_OPEN` / `variant-open` | 检查 expected Head，原子创建 Tx/Ticket/View，返回 owner UID/GID | [contract](../../CensorFs/crates/censorfs-core/src/control.rs)、[CLI](../../CensorFs/cmd/censorfs/src/main.rs) |
| 冻结变更 | `VARIANT_PREPARE` / `variant-prepare` | 关闭 View，产生 Candidate/Generation/Diff | [dispatcher](../../CensorFs/crates/censorfs-core/src/control.rs) |
| 提交 | `VARIANT_PUBLISH` / `variant-publish` | 按原 expected Head 做 CAS，幂等产生 Receipt | [dispatcher](../../CensorFs/crates/censorfs-core/src/control.rs) |
| 放弃 | `VARIANT_ABORT` / `variant-abort` | 关闭 View，Abort Ticket 和 Tx | [dispatcher](../../CensorFs/crates/censorfs-core/src/control.rs) |
| 把 View 变成 `/workspace` | `ATTACH_FUSE` | mounter 在私有 namespace 内挂载，通过 `SCM_RIGHTS` 传 `/dev/fuse` FD | [mounter](../../CensorFs/cmd/censorfs-mounter/src/main.rs)、[proto](../../CensorFs/api/censorfs.proto) |
| 收束批次后代进程 | mounter `--cgroup-root/--cgroup-state-dir/--cgroup-scope` | supervisor 清理 leaf cgroup，覆盖 setsid 和父进程退出；Pivot 已接入 | [cgroup](../../CensorFs/cmd/censorfs-mounter/src/cgroup.rs) |

底层控制面是 Unix Stream，使用 4-byte little-endian 长度前缀的 Protobuf
`ControlRequest/ControlResponse`。Pivot 初版通过已有 `censorfs --json` CLI 适配该协议，
不需要重新实现 CensorFS 客户端或文件系统逻辑。

## 3. CensorGuard

### 3.1 已有功能

| 能力 | 当前实现 |
|---|---|
| 内核强制 | eBPF LSM 对 file/exec/network 做 allow 或 `-EPERM`，DENY 始终记录 |
| 进程树域 | 根进程绑定 domain/group，fork 子孙自动继承 |
| 策略管理 | YAML 编译、文件/exec/network 规则、DNS 展开、双 bank 原子热更 |
| 启动防绕过 | launcher 在执行目标命令前登记自身，`deny` 故障策略下登记失败不 exec |
| 预判与解释 | `evaluate_intent` 按策略组评估 file/exec/network intent，返回规则和原因 |
| 审计 | `events.sock` 输出带 sequence/boot-id/drop 计数的实时事件 |

### 3.2 已有实现接口

| Pivot 需求 | 已有接口 | 线路与语义 | 代码依据 |
|---|---|---|---|
| 执行前快速拒绝 | RPC v3 `evaluate_intent` | `ctl.sock` JSON Lines；一次返回与 intents 等长的 decisions | [protocol](../../CensorGuard/crates/censorguard-common/src/protocol.rs)、[server](../../CensorGuard/crates/censorguard-daemon/src/server.rs) |
| 使 runner 进入受控域 | RPC v3 `register_self` | `launch.sock` 只接受该方法，daemon 用 `SO_PEERCRED` 绑定真实 PID | [server](../../CensorGuard/crates/censorguard-daemon/src/server.rs) |
| 无窗口启动 | `censorguard-exec` | 登记成功后原地 exec 工具树；`--failure-policy deny` 保持 fail-closed | [launcher](../../CensorGuard/crates/censorguard-exec/src/main.rs) |

`evaluate_intent` 是 Guard 已有的可选解释接口，不是安全边界。当前 Pivot 不使用该接口，
也不接收调用方 intent；实际安全边界统一为
`censorguard-exec -> register_self -> eBPF LSM`。

## 4. CensorScope

### 4.1 已有功能

| 能力 | 当前实现 |
|---|---|
| 被动采集 | procfs、eBPF tracepoint 和动态 uprobe，不要求被观测程序链接 SDK |
| 分级能力 | L1 进程/文件，L2 增加 mmap/网络/TLS，L3 增加 IPC/stdout/stderr |
| 进程树 Trace | 对已运行的 root PID 执行 `track-add`，按进程树采集 |
| 调用归因 | call span + `CENSORSCOPE_SESSION_ID` / `DSH_CENSORSCOPE_CALL_ID`，支持时间窗回补 |
| 存储与读取 | daemon 独占写 SQLite，对外提供只读视图和 `export` |

CensorScope 不执行放行策略，不得用它的可用性代替 CensorGuard 的安全结论。

### 4.2 已有实现接口

| Pivot 需求 | 已有接口 | 线路与语义 | 代码依据 |
|---|---|---|---|
| 建立批次 Trace | `TrackAdd` / `track-add --root-pid` | UDS 控制面，返回数值 `trace_id`；可指定 ID 继续旧 Trace | [contract](../../CensorScope/crates/contracts/control_plane/src/command.rs)、[CLI](../../CensorScope/crates/apps/ctl/src/args.rs) |
| 结束跟踪 | `TrackRemove` / `track-remove --trace-id` | 停止对对应 root 进程树的采集 | [contract](../../CensorScope/crates/contracts/control_plane/src/command.rs) |
| 标注工具开始 | `CallStart` / `call-start` | 关联 trace/session/call/host PID/开始时间 | [transport](../../CensorScope/crates/adapters/control/uds/transport/src/lib.rs) |
| 标注工具结束 | `CallEnd` / `call-end` | 状态仅接受 `success/error/cancelled/timeout`，结束标注持久后回复 | [service](../../CensorScope/crates/apps/daemon/src/service_host.rs) |
| 读取结果 | `trace-list`、`export`、SQLite `*_read` | 控制面查询与离线只读导出 | [README](../../CensorScope/README.md) |

控制面是 Unix Stream，使用 `length#value` 字段帧；Pivot 初版复用
`censorscopectl --json` 而不复制编解码器。

## 5. 接口结论与 Pivot 责任

### 5.1 三组件接口是否齐备

| 编排步骤 | 组件接口 | 结论 |
|---|---|---|
| 创建/冻结/发布/放弃文件世界 | CensorFS Variant 四接口 + `AttachFuse` | 齐备，Pivot 只做幂等适配和顺序组合 |
| 执行前意图预判 | CensorGuard `evaluate_intent` | Guard 接口齐备，但 Pivot 明确不接入 |
| 工具进程树强制 | `censorguard-exec` + `register_self` + eBPF LSM | 齐备，Pivot 负责保证所有工具只从该链启动 |
| 批次 Trace 和 call span | CensorScope `TrackAdd/Remove/CallStart/CallEnd` | 齐备，Pivot 负责用真实 runner PID 驱动生命周期 |
| 跨三组件的批次事务 | 无单一组件接口 | **必须由 Pivot 实现** |
| 一次请求包含 N 个工具调用 | 无单一组件接口 | **必须由 Pivot 实现** |
| 持久 Commit/Abort 决策与崩溃重放 | CensorFS 只保证自身请求幂等 | **必须由 Pivot 实现全局决策日志** |

### 5.2 没有明确接口时的原则

Pivot 只能实现“组合能力”：请求协议、身份校验、适配器、状态机、决策日志、
恢复器、runner 和超时/输出限制。如果缺少的是文件一致性、内核拦截或采集数据面能力，
应先在所属组件增加稳定接口，Pivot 再适配；不应在 Pivot 复制一份简化实现。

Pivot 当前已实现上表三项组合能力。runner 在经过 mounter 和 Guard launcher 后会先向
Pivot 回报真实 PID；Pivot 再用该 PID 调用 Scope `track-add`，从而避免把 `sudo`、
mounter 或 launcher 的中间 PID 误当成观测根。

## 6. 唯一 Demo 场景：原子生成并校验产物

场景只包含两个工具调用：

1. `generate-artifact` 在私有 `/workspace` 中生成 `demo/pivot-result.txt`；
2. `verify-artifact` 读回文件并校验精确内容。

两个调用共用一个 CensorFS View、一棵 CensorGuard 受控进程树和一个 CensorScope
Trace。只有两步都成功才会 `Prepare -> durable Commit decision -> Publish`；生成、策略、
观测或校验任一步失败都会 `durable Abort decision -> VariantAbort`。

请求模板为 [atomic-code-change.json](../examples/atomic-code-change.json)，实现为
[censorpivot-demo.rs](../src/bin/censorpivot-demo.rs)，便捷入口为
[`scripts/run-atomic-code-change-demo.sh`](../scripts/run-atomic-code-change-demo.sh)。Demo 只直读 CensorFS
Branch Head 元数据，工具调用仍必须经过 Pivot。
