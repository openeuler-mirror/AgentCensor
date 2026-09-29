# AgentCensor

AgentCensor 是面向 Agent 工作负载的主机级隔离、强制与观测系统。它把一次包含多个工具调用的 Agent 任务交给 **CensorPivot** 编排，在同一批次内同时提供：

- **隔离且可回滚的文件工作区**：由 [CensorFS](CensorFs/README.md) 提供私有 View、版本化提交和崩溃恢复；
- **内核级安全边界**：由 [CensorGuard](CensorGuard/README.md) 通过 eBPF LSM 对整棵进程树执行 file、exec、network 策略；
- **被动式运行观测**：由 [CensorScope](CensorScope/README.md) 采集进程、文件、网络、IPC 和工具调用归因数据；
- **统一入口与事务决策**：由 [CensorPivot](CensorPivot/README.md) 负责批次协议、生命周期、幂等、Commit/Abort 决策和故障恢复。

> **让 Agent 在隔离工作区中探索，让安全策略在内核中生效，让每次变更和调用都可以追溯。**

## 组件总览

AgentCensor 不是四个相互独立的工具，而是一条由 CensorPivot 串起的执行链：

```text
Agent Runtime
     │ BatchRequest
     ▼
┌──────────────┐
│ CensorPivot  │  校验、幂等、状态机、恢复、Commit/Abort
└──────┬───────┘
       │
       ├── CensorFS    私有 View → /workspace → Prepare/Publish 或 Abort
       ├── CensorGuard censorguard-exec → eBPF LSM → 进程树强制
       └── CensorScope track-add → CallStart/CallEnd → 事件与调用归因
```

| 组件 | 主要职责 | 文档 |
| --- | --- | --- |
| **CensorPivot** | 统一接入层和批次事务编排；把多个工具调用收束为一个可恢复的提交或放弃决定 | [组件 README](CensorPivot/README.md) · [安装与使用](CensorPivot/docs/INSTALL.zh-CN.md) · [完整运行流程](CensorPivot/docs/flow.md) · [设计与状态机](CensorPivot/docs/DESIGN.zh-CN.md) |
| **CensorFS** | 分支式、可回滚、可恢复的文件系统；为每个批次提供隔离的 `/workspace` | [组件 README](CensorFs/README.md) · [架构](CensorFs/docs/ARCHITECTURE.md) · [CLI 指南](CensorFs/docs/CensorFS_CLI_GUIDE.md) |
| **CensorGuard** | eBPF LSM 进程级安全强制；策略覆盖文件、执行和网络，并沿进程树继承 | [组件 README](CensorGuard/README.md) · [使用手册](CensorGuard/docs/使用手册.md) · [构建指南](CensorGuard/docs/构建指南.md) · [DSH 插件指南](CensorGuard/docs/DSH插件指南.md) |
| **CensorScope** | 无需被观测程序链接 SDK 的被动式主机观测和只读导出 | [组件 README](CensorScope/README.md) · [插件说明](CensorScope/plugins/README.md) |

组件接口和 Pivot 责任边界的逐项核对见 [组件功能与接口说明](CensorPivot/docs/COMPONENTS.zh-CN.md)。

## CensorPivot 如何编排一次批次

每个请求可以包含多个工具调用。Pivot 保证这些调用共享同一个文件 View、安全进程树和观测 Trace，并遵循“决策先落盘，第二阶段只重试原决定”的恢复原则。

```text
BatchRequest
    │
    ├─ 校验 request_id、工具路径、参数与资源限制
    ├─ CensorFS VariantOpen：创建私有 Ticket / View
    ├─ 启动 mounter 和 censorguard-exec
    │    ├─ 挂载私有 /workspace
    │    ├─ register_self：把 runner 加入 Guard domain
    │    └─ runner 回报真实 PID
    ├─ CensorScope track-add：从真实 runner PID 开始跟踪
    ├─ 顺序执行 Tool 1..N
    │    ├─ CallStart → 工具执行 → Guard 内核检查 → Scope 采集 → CallEnd
    │    └─ 任一步失败，批次进入 Abort 路径
    ├─ CensorFS VariantPrepare：冻结 View，生成 Candidate
    └─ 持久化唯一决定
         ├─ Commit → VariantPublish → Branch Head 前进
         └─ Abort  → VariantAbort   → 丢弃私有结果
```

这里的三种身份分别由三个底层组件建立：CensorFS 的 Ticket/View 决定文件世界，CensorGuard 的 domain/group 决定可执行边界，CensorScope 的 trace/session/call 决定观测归属。Pivot 负责保证它们使用同一个真实 runner 和同一批次生命周期。

## 快速开始

### 环境要求

完整部署需要 Linux、systemd、cgroup v2、FUSE、BTF，以及启用 BPF LSM 的内核。构建 CensorGuard 和 CensorScope 还需要 Rust、clang、libbpf 头文件等工具。CensorFS 的持久化目录应位于本地 ext4 或 XFS；完整检查项见 [CensorPivot 安装文档](CensorPivot/docs/INSTALL.zh-CN.md)。

### 一键安装原生组件

在仓库根目录执行。安装器会按 CensorPivot 所需顺序构建并安装 CensorFS、CensorGuard、CensorScope、统一 daemon 和 Pivot 服务：

```bash
cd /path/to/AgentCensor
sudo CensorPivot/scripts/install-agentcensor.sh all
sudo CensorPivot/scripts/install-agentcensor.sh start
```

首次安装或排障时先运行：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh doctor
```

常用运维命令：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh status
sudo CensorPivot/scripts/install-agentcensor.sh doctor
sudo CensorPivot/scripts/install-agentcensor.sh stop
```

也可以分别安装：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh fs
sudo CensorPivot/scripts/install-agentcensor.sh guard
sudo CensorPivot/scripts/install-agentcensor.sh scope
sudo CensorPivot/scripts/install-agentcensor.sh pivot
```

安装器会创建 `censorpivot` 系统用户、配置文件、systemd unit 和所需的运行目录。不要在没有完成 `pivot` 安装时直接以 `censorpivot` 用户启动服务。

### 手工构建

```bash
cd CensorFs && cargo build --release
cd ../CensorGuard && make bpf && cargo build --workspace --release
cd ../CensorScope && cargo build --release -p daemon -p ctl
cd ../CensorPivot && cargo build --release && cargo test
```

单独构建只生成当前组件的产物，不会创建系统用户、安装 daemon 或准备内核环境。生产部署应使用上面的统一安装器和 [安装文档](CensorPivot/docs/INSTALL.zh-CN.md)。

## 运行 CensorPivot

### DeepSeek Harness 集成

CensorPivot 的主要使用场景是为 DeepSeek Harness（DSH）提供隔离工作区、安全策略和运行观测。请先准备 [DeepSeek Harness 仓库](https://github.com/deepseek-ai/deepseek-harness)，并固定使用 `0.1.5-rc.2`（对应 tag `dsh-v0.1.5-rc.2`）版本。不要直接使用其它版本的 DSH：当前 CensorFS、CensorGuard、CensorScope 和 CensorPivot 插件依赖这一版本的包接口。

```bash
git clone https://github.com/deepseek-ai/deepseek-harness.git
cd deepseek-harness
git checkout dsh-v0.1.5-rc.2
pnpm install
```

完成 AgentCensor 原生组件安装并启动 daemon 后，在仓库根目录安装 DSH 组合插件：

```bash
CensorPivot/scripts/install-dsh-censorpivot.sh
```

最后回到 DSH 仓库根目录，用 `pnpm web` 启动 Web 入口：

```bash
cd /path/to/deepseek-harness
pnpm web
```

组合插件会将 CensorFS、CensorGuard、CensorScope 接入 DSH 的 `web` 和 `headless` profile。完整环境变量、源码目录识别、打包和排障说明见 [CensorPivot 安装文档](CensorPivot/docs/INSTALL.zh-CN.md) 和 [DSH 集成说明](CensorFs/integrations/deepseek-harness/README.md)。

### 原生 CLI 与 Demo

不通过 DSH 时，统一安装后的 Pivot 主要 CLI 入口是：

```bash
censorpivot submit --file CensorPivot/examples/batch.json
censorpivot status <TRANSACTION_ID>
censorpivot recover
censorpivot doctor
```

提交协议中的工具必须使用绝对路径，工作目录只能是 `/workspace` 下的相对路径。示例批次见：

- [原子代码变更示例](CensorPivot/examples/atomic-code-change.json)
- [批次请求示例](CensorPivot/examples/batch.json)
- [运行 Demo](CensorPivot/scripts/run-atomic-code-change-demo.sh)

在三个组件和 Pivot daemon 均已启动、`censorpivot doctor` 返回 ready 后，可以运行完整 Demo：

```bash
cd CensorPivot
CENSORPIVOT_DEMO_GUARD_GROUP=censorguard-dsh-default \
  scripts/run-atomic-code-change-demo.sh
```

该 Demo 在一个私有 View 中生成并校验文件。两步全部成功才发布 Branch Head，任何生成、策略、观测或校验失败都会放弃本批次。

## 安全边界与已知限制

- CensorGuard 默认 fail-closed；CensorScope 是观测组件，不能替代 Guard 的安全结论。
- CensorPivot 的原子性覆盖 CensorFS 文件修改和自身 Commit/Abort 决策，不覆盖网络请求、外部数据库、消息发送等带外副作用。
- CensorFS 当前明确不支持 symlink、hardlink、xattr、ACL、设备节点、FIFO、socket、chown、共享可写 mmap、在线 GC 和跨分支原子提交。
- 组件 daemon 和控制面依赖 Unix socket、合适的 UID/GID、`/dev/fuse`、cgroup v2、BTF 与 BPF LSM；缺少这些条件时应先看 `doctor` 输出和 [部署文档](CensorPivot/docs/INSTALL.zh-CN.md)。

更完整的崩溃矩阵、信任边界和接口约束见 [CensorPivot 设计文档](CensorPivot/docs/DESIGN.zh-CN.md)、[运行流程](CensorPivot/docs/flow.md) 和 [代码审查记录](CensorPivot/docs/REVIEW.zh-CN.md)。

## 开发与测试

各组件拥有独立的 Cargo workspace 和测试入口。修改跨组件协议或 Pivot 编排逻辑时，建议至少运行：

```bash
(cd CensorFs && cargo test)
(cd CensorGuard && cargo test --workspace --offline)
(cd CensorScope && cargo test --workspace --lib --bins)
(cd CensorPivot && cargo test)
```

需要真实 FUSE、eBPF、mount namespace 或 BPF LSM 的验收必须在满足内核条件的 Linux 主机执行，普通开发机上的单元测试不能替代该验收。

## 许可证

MulanPSL-2.0 —— see LICENSE for details.


## 致谢

感谢 **AcTrail** 项目及其作者提供的设计启发。AgentCensor 在进程树追踪集合维护、运行时观测与事件生命周期管理等方面参考了 AcTrail 的思路，并结合 CensorFS 的可恢复文件状态、CensorGuard 的内核强制和 CensorPivot 的批次事务模型形成当前实现。相关借鉴会继续在代码注释和设计文档中保持可追溯；如需了解具体边界，请参阅 [完整运行流程](CensorPivot/docs/flow.md) 与 [组件接口说明](CensorPivot/docs/COMPONENTS.zh-CN.md)。

同时感谢 Rust、Linux、eBPF、FUSE、systemd、SQLite 以及 DeepSeek Harness 社区提供的基础设施和工具。
