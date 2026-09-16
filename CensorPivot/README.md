# CensorPivot

## 从零安装

CensorPivot 运行时依赖 CensorFS、CensorGuard、CensorScope，单独执行 `cargo build` 不会
安装这三个组件，也不会创建 `censorpivot` 系统用户。推荐从仓库根目录执行：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh fs
sudo CensorPivot/scripts/install-agentcensor.sh guard
sudo CensorPivot/scripts/install-agentcensor.sh scope
sudo CensorPivot/scripts/install-agentcensor.sh pivot
sudo CensorPivot/scripts/install-agentcensor.sh start
```

也可用 `all` 一次安装四个项目。系统要求、首次基线导入、状态检查、停止和故障排查见
[从零安装与使用文档](docs/INSTALL.zh-CN.md)。

## 最小统一 daemon

`censord` 是当前最小运行入口，只管理 CensorFS、CensorGuard、CensorScope 的生命周期，
不执行下面的批次事务流程。先按部署路径修改 `censord.example.json`，再运行：

```bash
sudo install -d -m 0750 /etc/agentcensor
sudo install -m 0640 censord.example.json /etc/agentcensor/censord.json
sudo target/release/censord init
sudo target/release/censord run
sudo target/release/censord doctor
```

`run` 保持在前台并直接监督三个组件。收到 `SIGINT`/`SIGTERM`、任一组件异常退出或启动
健康检查失败时，它会先向三个独立进程组发送 `SIGTERM`，超时后发送 `SIGKILL` 并回收
子进程。组件还设置父进程死亡信号，避免 `censord` 被强制终止后继续运行。

`init` 可重复执行：已有 CensorFS superblock 时不会再次导入基线；Guard 配置只做静态校验；
Scope 配置不存在时创建、存在时校验。`doctor` 检查本地文件、Unix socket，并分别调用
FS `info`、Guard `doctor`、Scope `doctor`。当前不调用 Variant 或批次执行接口。

## DSH 组合插件

CensorPivot 提供一个 DSH 安装入口，把 CensorFS、CensorGuard、CensorScope 的现有插件按
1+1+1 方式组合起来。它不合并或重写三个插件的实现：Web profile 只激活公开的
`@agentcensor/censorpivot` 聚合层，安装脚本同时为 Headless profile 配置内部的
`@agentcensor/censorpivot-headless` 配套层。各组件包仍作为普通运行时依赖安装，因此不会
重复应用原来的 bundle patch，浏览器端的 `dsh.client` 声明则继续生效。

已安装 Node.js 22+、pnpm 和 dsh 后，在仓库根目录执行：

```bash
CensorPivot/scripts/install-dsh-censorpivot.sh
```

脚本会在需要时自动生成八个本地 tarball，然后配置 `web` 和 `headless` 两个 profile。
可用 `DSH_HOME` 指定独立的 DSH 数据目录，用 `DSH_BIN=/path/to/dsh` 指定可执行文件，
或用 `DSH_ROOT=/root/deepseek-harness` 指定源码目录。变量赋值的 `=` 两边不能有空格。
安装器也会自动查找当前目录及 AgentCensor 相邻的 `deepseek-harness/`。用
`CENSORPIVOT_PACKAGE_DIR` 可显式复用预先生成的包；未设置时每次安装都会从当前源码重新
打包，避免重复安装仍使用旧 `dist/`。安装器默认使用 pnpm 的标准 store；全局 store 不可写
时可用 `DSH_PNPM_STORE_DIR` 指定私有目录。只生成包、不安装时执行：

```bash
CensorPivot/scripts/package-dsh-censorpivot.sh
```

插件安装不代替系统组件部署。`censorfs`、`censorfs-mounter`、CensorGuard 和
CensorScope daemon 及其 socket 必须已经按各自文档安装并启动。安装脚本会把 CensorFS
的 `dsh-jsonrpc-agent` 放到 `${AGENTCENSOR_BIN_DIR:-$HOME/.local/bin}`；启动 dsh 前该目录
必须位于 `PATH`。Guard 的整棵进程树策略由 Web 进程继承给 Headless 子进程，所以不需要
额外的 Guard Headless 插件。

CensorPivot 是 AgentCensor 的编排与接入层。调用方把一批工具调用作为一个请求交给
本地 Unix Socket；CensorPivot 在一个 CensorFS 私有 View 中顺序执行它们，用
CensorGuard 对整棵进程树做内核强制，并向 CensorScope 标记每个调用。整批成功后冻结
Candidate、持久化 Commit 决策并发布；任一步失败则持久化 Abort 决策并清理。

核心约束是：**决策先落盘，第二阶段后执行；一旦决定，只能重试原决定。**

## 构建与测试

```bash
cd CensorPivot
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

## 运行

统一安装器会部署 `config.example.json`。手工部署时应按实际路径修改；`censorpivot` 应以
可信网关身份运行，具备：

- 连接 CensorFS control socket、执行受控 `censorfs-mounter` 的权限；
- 执行 `censorguard-exec` 并访问 Guard launch socket；
- 按配置访问 CensorScope control socket；
- 独占写入 `state_dir`。

`serve` 拒绝 root UID；View 的 owner UID/GID 也必须非零。`censorguard.allowed_groups`
由管理员配置，请求只能选择其中的组。默认组为 `censorguard-dsh-default`；统一安装器会在
`start` 时自动下发，手工部署则需先在 Guard 安装。
实际 domain 名称由事务 UUID 生成，返回记录的 `summary.guard_scope` 保存该名称。

部署还需 `/dev/fuse` 和 cgroup v2。管理员须准备配置中的 `cgroup_root`（默认
`/sys/fs/cgroup/censorpivot`）；mounter 使用其已有的 supervisor 功能创建独立 leaf cgroup，
在批次结束或父进程退出时清理后代。`cgroup_state_dir` 必须由 root 管理，工具不可写。
`runner_timeout_ms` 控制启动握手、发送与结束等待；`component_timeout_ms` 控制 FS/Scope CLI。
`doctor` 检查这些环境条件，并实际调用 FS `info` 和 Scope `doctor`，但不主动创建 Guard domain。

不要在尚未创建 `censorpivot` 用户时直接执行带 `-o censorpivot` 的 `install` 命令。完整部署
应使用 `scripts/install-agentcensor.sh pivot`；需要前台调试时再执行：

```bash
sudo -u censorpivot /usr/local/bin/censorpivot serve --config /etc/censorpivot/config.json
```

提交批次：

```bash
target/release/censorpivot submit --file examples/batch.json
target/release/censorpivot status <TRANSACTION_ID>
target/release/censorpivot recover
target/release/censorpivot doctor
```

CLI 的退出码：`0` 表示已提交/恢复完成，`2` 表示事务按规则终止，`3` 表示已有
Commit/Abort 决策但第二阶段仍待重试，`1` 表示协议或接入错误。

## 重要边界

- 事务原子性覆盖 CensorFS 中的文件修改与 CensorPivot 的决策，不承诺回滚网络请求、
  外部数据库、消息发送等带外副作用。
- 所有工具程序必须使用绝对路径；工作目录只能是 `/workspace` 下的相对路径。
- CensorGuard 默认 fail-closed。CensorScope 可配置为 required；非 required 时观测失败
  不会改变安全或文件提交结论，但会写入事务 `warnings`。
- `request_id` 是调用方幂等键。相同 ID、相同内容返回原事务；相同 ID、不同内容拒绝。

完整协议、状态机、崩溃矩阵与部署信任边界见 [设计文档](docs/DESIGN.zh-CN.md)。
三个底层组件的现有功能、真实接口和 Pivot 责任划分见
[组件功能与接口说明](docs/COMPONENTS.zh-CN.md)。
从 Guard 策略生效、CensorFS 私有挂载到 Scope 进程树跟踪的逐步说明见
[完整运行流程](docs/flow.md)。
接口核对、修复及验证范围见 [代码审查记录](docs/REVIEW.zh-CN.md)。

## 单场景 Demo

Demo 在同一个私有 View 中先生成 `demo/pivot-result.txt`，再校验文件内容。
两步全部成功才发布 Branch Head，任一步失败都 Abort。

前置条件：CensorFS、CensorGuard、CensorScope 和 CensorPivot daemon 均已启动，
`censorpivot doctor` 返回 ready，当前 Guard 策略组允许 Demo 的 `/bin/sh` 与
`/workspace/demo` 读写。Demo 会额外要求 doctor 的所有组件项均为 `ok`，
并验证事务返回了 CensorScope trace ID。

```bash
cd CensorPivot
cargo build --release
CENSORPIVOT_DEMO_GUARD_GROUP=censorguard-dsh-default \
  scripts/run-atomic-code-change-demo.sh
```

可通过 `CENSORPIVOT_SOCKET`、`CENSORFS_SOCKET`、`CENSORPIVOT_DEMO_BIN`、`CENSORFS_BIN`和
`CENSORPIVOT_DEMO_BRANCH` 覆盖默认值。脚本会自动读取当前 Branch Head，生成唯一
幂等键，提交批次，然后验证事务终态和 Head 确已前进。

不具备内核环境时，可以执行同一示例中的真实 shell 工具及 launcher 协议验证：

```bash
cargo test two_call_artifact_example_runs_with_real_tools
CENSORGUARD_EXEC=../CensorGuard/target/debug/censorguard-exec \
  cargo test --test guard_launcher -- --ignored
```

后者使用真实 Guard launcher 和 Pivot runner、临时 Unix socket 与模拟注册服务，验证拒绝时
不 exec、允许时 PID 不变；它不加载 eBPF，也不挂载 FUSE。
