<div align="center">

<h1>AgentCensor</h1>

<p><strong>面向 Agent 的文件隔离、安全治理与运行观测</strong></p>

<p>在私有工作区中探索，在内核边界内执行，让工具调用与文件变更可以追溯。</p>

<p>
  <code>Linux</code> · <code>Rust</code> · <code>eBPF LSM</code> · <code>FUSE</code>
</p>

<p>
  <a href="#components">组件总览</a> ·
  <a href="#quick-start">快速开始</a> ·
  <a href="#deepseek-harness">DeepSeek Harness</a> ·
  <a href="#documentation">使用文档</a> ·
  <a href="#acknowledgements">致谢</a>
</p>

</div>

---

AgentCensor 是面向 Agent 工作负载的主机级隔离、强制与观测系统。以 **CensorPivot** 为编排入口，将 **CensorFS** 的私有文件工作区、**CensorGuard** 的内核策略和 **CensorScope** 的运行观测接入同一批次。多个工具调用共享执行边界，文件修改通过持久化的 Commit/Abort 决策发布或放弃。

**首次使用？** 从 [统一安装](#quick-start) 开始，再通过 [DeepSeek Harness](#deepseek-harness) 接入 Agent。

<a id="components"></a>

## Components · 组件总览

<table width="100%">
  <thead>
    <tr>
      <th width="25%">事务编排</th>
      <th width="25%">文件隔离</th>
      <th width="25%">安全强制</th>
      <th width="25%">运行观测</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td valign="top"><strong><a href="CensorPivot/README.md">CensorPivot</a></strong><br><sub>统一接入与批次事务</sub></td>
      <td valign="top"><strong><a href="CensorFs/README.md">CensorFS</a></strong><br><sub>可分支、可回滚的工作区</sub></td>
      <td valign="top"><strong><a href="CensorGuard/README.md">CensorGuard</a></strong><br><sub>eBPF LSM 进程树保护</sub></td>
      <td valign="top"><strong><a href="CensorScope/README.md">CensorScope</a></strong><br><sub>被动采集与工具调用归因</sub></td>
    </tr>
    <tr>
      <td valign="top">批次请求与幂等<br>持久化 Commit/Abort<br>生命周期与故障恢复</td>
      <td valign="top">私有 View 与 FUSE<br>版本化提交与分支合并<br>回滚与崩溃恢复</td>
      <td valign="top">文件、执行与网络策略<br>子进程继承安全域<br>策略热更新与审计</td>
      <td valign="top">进程、文件与网络事件<br>会话与调用级关联<br>SQLite 查询与导出</td>
    </tr>
    <tr>
      <td valign="top"><a href="docs/zh/user_guide/deployment_guide/deployment.md">安装与启动 →</a><br><a href="docs/zh/user_guide/usage_guide/censorpivot.md">运行指南</a></td>
      <td valign="top"><a href="docs/zh/user_guide/usage_guide/censorfs_cli.md">命令行指南 →</a><br><a href="docs/zh/user_guide/deepseek_harness_guide/integration.md">DSH 集成</a></td>
      <td valign="top"><a href="docs/zh/user_guide/usage_guide/censorguard.md">策略与使用 →</a><br><a href="docs/zh/user_guide/component_guide/censorguard_dsh.md">DSH 插件</a></td>
      <td valign="top"><a href="docs/zh/user_guide/component_guide/censorscope.md">观测指南 →</a><br><a href="docs/zh/user_guide/usage_guide/censorscope_plugins.md">DSH 插件</a></td>
    </tr>
  </tbody>
</table>

**一次批次，三种身份：** CensorPivot 将文件 View、安全 domain 和观测 Trace 关联到同一个 runner，顺序执行工具，再协调文件结果的发布或放弃。

[查看完整运行流程 →](CensorPivot/docs/flow.md) · [组件功能与接口说明](CensorPivot/docs/COMPONENTS.zh-CN.md)

<details>
<summary><strong>展开：CensorPivot 如何编排一次批次</strong></summary>

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

</details>

<a id="quick-start"></a>

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

<details>
<summary><strong>按组件安装与手工构建</strong></summary>

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

</details>

<a id="deepseek-harness"></a>

## 运行 CensorPivot · DeepSeek Harness

### 1. 准备 DeepSeek Harness

CensorPivot 的主要使用场景是为 DeepSeek Harness（DSH）提供隔离工作区、安全策略和运行观测。请提前安装 Node.js 22+ 和 pnpm，准备 [DeepSeek Harness 仓库](https://github.com/deepseek-ai/deepseek-harness)，并固定使用 `0.1.5-rc.2`（对应 tag `dsh-v0.1.5-rc.2`）版本。不要直接使用其它版本的 DSH：当前 CensorFS、CensorGuard、CensorScope 和 CensorPivot 插件依赖这一版本的包接口。

```bash
git clone https://github.com/deepseek-ai/deepseek-harness.git
cd deepseek-harness
git checkout dsh-v0.1.5-rc.2
pnpm install
```

### 2. 安装 CensorPivot 组合插件

完成 AgentCensor 原生组件安装并启动 daemon 后，回到 **AgentCensor 仓库根目录**安装组合插件：

```bash
cd /path/to/AgentCensor
DSH_ROOT=/path/to/deepseek-harness \
  CensorPivot/scripts/install-dsh-censorpivot.sh
```

### 3. 启动 Web

回到 **DeepSeek Harness 仓库根目录**，用 `pnpm web` 启动：

```bash
cd /path/to/deepseek-harness
pnpm web
```

组合插件会将 CensorFS、CensorGuard、CensorScope 接入 DSH 的 `web` 和 `headless` profile。完整环境变量、源码目录识别、打包和排障说明见 [CensorPivot 安装文档](CensorPivot/docs/INSTALL.zh-CN.md) 和 [DSH 集成说明](CensorFs/integrations/deepseek-harness/README.md)。

<details>
<summary><strong>原生 CLI 与批次 Demo</strong></summary>

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

</details>

## 安全边界与已知限制

- CensorGuard 默认 fail-closed；CensorScope 是观测组件，不能替代 Guard 的安全结论。
- CensorPivot 的原子性覆盖 CensorFS 文件修改和自身 Commit/Abort 决策，不覆盖网络请求、外部数据库、消息发送等带外副作用。
- CensorFS 当前明确不支持 symlink、hardlink、xattr、ACL、设备节点、FIFO、socket、chown、共享可写 mmap、在线 GC 和跨分支原子提交。
- 组件 daemon 和控制面依赖 Unix socket、合适的 UID/GID、`/dev/fuse`、cgroup v2、BTF 与 BPF LSM；缺少这些条件时应先看 `doctor` 输出和 [部署文档](CensorPivot/docs/INSTALL.zh-CN.md)。

更完整的崩溃矩阵、信任边界和接口约束见 [CensorPivot 设计文档](CensorPivot/docs/DESIGN.zh-CN.md)、[运行流程](CensorPivot/docs/flow.md) 和 [代码审查记录](CensorPivot/docs/REVIEW.zh-CN.md)。

<a id="documentation"></a>

## 使用文档

| 从这里开始 | 内容 |
| --- | --- |
| [用户指南目录](docs/zh/user_guide/_toc.yaml) | 项目简介、部署、运行与各组件使用说明 |
| [安装与启动](docs/zh/user_guide/deployment_guide/deployment.md) | 环境准备、统一部署、状态检查和故障排查 |
| [DeepSeek Harness 集成](docs/zh/user_guide/deepseek_harness_guide/integration.md) | 插件安装、Parallel Worlds 和运行配置 |
| [批次运行流程](CensorPivot/docs/flow.md) | 私有工作区、安全进程树与观测 Trace 的协作过程 |

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

本项目采用 [MulanPSL-2.0](LICENSE) 许可证。

<a id="acknowledgements"></a>

## 致谢

感谢 **AcTrail** 项目及其作者提供的设计启发。AgentCensor 在进程树追踪集合维护、运行时观测与事件生命周期管理等方面参考了 AcTrail 的思路，并结合 CensorFS 的可恢复文件状态、CensorGuard 的内核强制和 CensorPivot 的批次事务模型形成当前实现。相关借鉴会继续在代码注释和设计文档中保持可追溯；如需了解具体边界，请参阅 [完整运行流程](CensorPivot/docs/flow.md) 与 [组件接口说明](CensorPivot/docs/COMPONENTS.zh-CN.md)。

同时感谢 Rust、Linux、eBPF、FUSE、systemd、SQLite 以及 DeepSeek Harness 社区提供的基础设施和工具。
