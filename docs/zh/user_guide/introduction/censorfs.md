<div align="center">

<h1>CensorFS</h1>

<h3>面向多 Agent 的可分支、可回滚、可恢复文件系统</h3>

<p>
  <code>v0.1</code>
  · <code>openEuler 24.03</code>
  · <code>Linux 6.6 / 6.12</code>
  · <code>AArch64 / 鲲鹏</code>
  · <code>Rust 1.82</code>
</p>

<p>
  <strong>多分支探索</strong> ·
  <strong>版本化提交</strong> ·
  <strong>崩溃治理</strong> ·
  <strong>FUSE 隔离</strong>
</p>

<p>
  <a href="#面向场景">面向场景</a> ·
  <a href="#功能清单">功能清单</a> ·
  <a href="#后续路线图">路线图</a> ·
  <a href="#快速开始">快速开始</a> ·
  <a href="#代码结构">代码结构</a> ·
  <a href="#文档">完整文档</a>
</p>

</div>

---

> **让探索彼此隔离，让提交完整留痕，让崩溃可以治理。**

CensorFS 是面向多 Agent 工作负载的分支式文件系统。多个 Agent 可以从同一个稳定版本创建相互隔离的私有视图，在各自的 `/workspace` 中探索、提交或放弃修改。每次发布都会生成不可变的 `Generation`，分支只通过带序号的 CAS `Branch Head` 向前推进。

```text
                          ┌─ Agent A ─ Ticket A ─ Commit ─ Branch A
Stable Generation ───────┼─ Agent B ─ Ticket B ─ Abort
                          └─ Agent C ─ Ticket C ─ Commit ─ Branch C
                                                        │
                                      Merge / Rollback ─┘
```

| 使用入口 | 适用范围 |
|---|---|
| `censorfs` / `censorfs-*` | 通过 Unix Socket 操作核心引擎，适合功能演示、自动化测试和故障检查 |
| `censorfsd + censorfs-mounter` | 通过 FUSE 和独立 Mount Namespace 为 Agent 提供真实的 `/workspace` 数据面 |

DeepSeek Harness 的 Parallel Worlds MVP 已作为可安装 bundle 放在 [`integrations/deepseek-harness`](../../../../CensorFs/integrations/deepseek-harness/README.md)：`/explore 3 <task>` 会从同一 Head 启动三个完整、隔离、可运行的 Harness 世界，冻结 Candidate、在只读 View 中验证，并由用户通过 CAS 只发布冠军。

## 面向场景

CensorFS 适合需要“让多个 Agent 放开探索，同时保证结果可隔离、可审计、可恢复”的工作负载：

| 场景 | CensorFS 提供的能力 |
|---|---|
| 多分支并行探索 | 多个 Agent 从同一 Generation 创建独立 Branch/Ticket，各自修改 `/workspace`，未提交结果互不可见 |
| 推测执行与失败试验 | Agent 可以反复创建私有探索；验证失败时 Abort Ticket，稳定分支内容和 Head 不变 |
| 受控成果发布 | 通过 `Prepare -> Candidate -> Publish` 固化成果，并以 Branch Head CAS 防止并发提交静默覆盖 |
| 多 Agent 成果汇聚 | 使用共同祖先三方 Merge 合并不同路径的修改；发生冲突时保持目标分支不变 |
| 版本审计与回滚 | 每次发布生成不可变 Generation；已发布回滚创建新的 Rollback Generation，不改写历史 |
| 崩溃治理 | daemon 重启时恢复 Journal、Head、Candidate 和 Receipt；未 Prepare 的开放 Ticket 被安全 Abort，复合请求可以幂等续跑 |
| 离线故障处置 | daemon 停止后使用 `censorfs-fsck` 检查或执行可证明安全的修复，无法安全修复时保持只读 |
| Agent 运行隔离 | 每个 Agent 使用独立 Mount Namespace，只挂载 `/workspace`，不能直接访问 `.censorfs` |

典型应用包括代码生成与验证、数据处理试验、自动化修复、多个 Agent 对同一工程的并行方案探索，以及需要保留完整变更链路的长时间 Agent 任务。

## 功能清单

### 已实现

| 类别 | 已实现特性 |
|---|---|
| 分支与版本 | 不可变 Generation、完整排序 Manifest、独立 Branch Head、`head_seq` CAS、历史版本读取与 Diff |
| 私有探索 | `Ticket Upper + Delta`、copy-up、Explore、Prepare、Commit、Abort，以及多个 View 的内容与 inode 隔离 |
| 发布与回滚 | `Prepare -> Candidate -> Publish`、发布 Receipt、未发布 Abort、生成新版本的已发布回滚 |
| Merge | 唯一最近共同祖先、逐路径三方 Merge、双父 Generation、冲突排序报告和 `--check` 只检查模式 |
| 崩溃恢复 | 带序号和 CRC32C 的 Journal、持久请求幂等表、启动恢复、开放 Ticket 清理、缺失 Receipt/关联恢复 |
| 元数据冗余 | Superblock 与 Branch Head 使用本地 A/B 双槽，启动时选择最新有效槽 |
| 数据完整性 | Object/Manifest/Generation BLAKE3 校验、固定版本小端编码、原子 rename、文件及父目录 fsync |
| 离线检查 | `censorfs-fsck` 只读检查与有限安全修复；损坏 Object 或不可判定 Head 不进行猜测性修复 |
| 数据面 | 可测试 `ViewEngine`、Linux FUSE 适配、Ticket `direct_io`、只读稳定/历史/Candidate View |
| 隔离与安全 | Unix Socket `SO_PEERCRED`、View 所有权、`openat2` 路径约束、独立 Mount Namespace、mounter 降权 |
| 文件操作 | 普通文件、目录、读写、truncate、chmod、utimens、rename、unlink、rmdir、readdir 和 fsync |
| 工具与测试 | `censorfs-*` 可读命令、故障注入、多分支/Merge/Abort/fsck 脚本、openEuler FUSE smoke 脚本 |

### 尚未实现的边界

v0.1 尚不支持 symlink、hardlink、xattr、ACL、设备节点、FIFO、socket、chown、共享可写 mmap、文本行级自动合并、在线 GC、跨分支原子提交和在线磁盘格式升级。这些限制是明确的产品边界，不应通过绕过校验用于生产。

## 后续路线图

下面是工程方向，不代表已经实现或已经稳定的外部接口。优先级首先考虑可验证的可靠性，其次才是容量和吞吐优化。

### 第一阶段：生产加固与可运维性

- **性能基线与优化**：建立元数据密集、大小文件、并发 View、Prepare/Publish 和恢复耗时基准；据此优化路径锁粒度、Manifest 构建、Object 读写、缓存、Journal 批处理和 fsync 次数。
- **GC**：实现从所有 Branch Head、Candidate、Receipt 和保留策略出发的可达性分析；先提供只报告模式，再采用标记、隔离、延迟删除的多阶段回收，避免与并发发布竞争。
- **Superblock 备份与恢复**：当前 A/B 双槽只解决实例内单槽损坏和写入中断；后续增加独立备份副本、可校验导出、恢复演练和灾难恢复工具。
- **持续完整性巡检**：后台或离线 scrub Generation、Manifest、Object 和引用关系，支持按速率限制的 BLAKE3 复验、坏对象定位和健康报告。
- **文件系统加固**：补充断电与写缓存测试、模糊测试、系统调用故障注入、磁盘满/只读/介质错误处理，以及 SELinux 策略、seccomp、能力最小化和安全审计。
- **资源治理与可观测性**：增加 Branch/Ticket/Object 数量和容量配额、并发与请求限流、运行指标、恢复状态、GC 状态、慢操作日志和告警接口。

### 第二阶段：规模与性能特性

- **可增量 Manifest**：评估 Merkle/分块 Manifest、增量 Diff 和增量 Prepare，避免大型目录树每次提交都重写完整 Manifest。
- **大文件优化**：分块 Object、稀疏文件、顺序读预取、零拷贝路径，以及在 XFS/ext4 能力允许时使用 reflink 加速 copy-up。
- **内容寻址与去重**：在不改变引用完整性和恢复语义的前提下，增加内容寻址索引、跨 Generation Object 去重和安全引用计数。
- **缓存与并行 I/O**：为稳定只读 View 增加受控页缓存和元数据缓存，改进多 Branch 并行 Prepare/Publish，同时保持单 Branch CAS 顺序。
- **索引与查询**：增加 Generation 图索引、快速共同祖先查询、路径历史、审计查询和大规模 Branch 枚举。

### 第三阶段：高级版本与协作能力

- **高级 Merge**：可插拔文本/结构化合并驱动、冲突工作区、rename 感知和人工确认流程；自动解决必须保持可审计和可重放。
- **原子 Branch Set**：为确有需求的场景设计多分支原子发布协议，而不是把当前单 Branch CAS 隐式扩展为伪原子操作。
- **签名与可信版本**：Generation/Receipt 签名、策略化发布审批、密钥轮换和可验证审计链。
- **备份、复制与灾备**：一致性快照、增量导出/导入、远端 Object 复制、恢复点验证和跨机器灾难恢复。
- **在线升级**：带回滚保护的磁盘格式迁移、daemon 升级和 FUSE Session 生命周期治理。
- **更完整的 Linux 语义**：在安全模型明确后按需支持 xattr、ACL、受控 symlink/hardlink、配额和更完整的 mmap 语义。
- **多架构交付**：在保持磁盘格式兼容的前提下增加 x86-64 构建、发行包、兼容性矩阵和持续集成。

## 支持平台

正式构建目标如下：

| 项目 | 要求 |
|---|---|
| 服务器 | 鲲鹏或其他 AArch64 服务器 |
| 操作系统 | openEuler 24.03 LTS |
| 内核 | Linux 6.6 或 6.12；最低要求为 6.6 |
| Rust | 1.82.0，由 `rust-toolchain.toml` 固定 |
| 持久化目录 | 同一块本地 XFS 或 ext4 |
| FUSE 数据面 | 内核 FUSE、`/dev/fuse`、`fuse3` 用户态工具 |

NFS、CIFS、OverlayFS、tmpfs 和 FUSE 文件系统不能作为生产持久化目录。磁盘记录采用固定宽度小端编码，不依赖 CPU 内存布局，因此 AArch64 与 x86-64 可以读取同一格式的数据；不同架构仍需分别编译可执行文件。

## 一键构建

在 openEuler 24.03 AArch64 服务器的源码根目录执行：

```bash
bash scripts/build-openeuler-aarch64.sh --install-deps
```

脚本会：

1. 检查 AArch64、openEuler 和 Linux 6.6 以上内核；
2. 使用 `dnf` 安装 GCC、构建工具、FUSE、`jq` 等依赖；
3. 安装并固定 Rust 1.82.0；
4. 使用 `Cargo.lock` 执行 release 构建；
5. 在 `target/release` 中为全部 `censorfs-*` 命令创建指向同一个 `censorfs` 二进制的符号链接。

构建产物位于 `target/release/`：

```text
censorfs
censorfsd
censorfsctl
censorfs-mounter
censorfs-init -> censorfs
censorfs-info -> censorfs
...其余 censorfs-* 命令均指向 censorfs
```

运行完整测试需要提供位于本地 XFS/ext4 的临时目录：

```bash
mkdir -p /data/censorfs-test-tmp
CENSORFS_TEST_TMPDIR=/data/censorfs-test-tmp \
  bash scripts/build-openeuler-aarch64.sh --with-tests
```

完整依赖说明、离线构建和部署检查见 [openEuler 鲲鹏构建与部署](../../../../CensorFs/docs/OPENEULER_AARCH64.md)。

## 快速开始

下面的例子只使用可读命令集，不创建 FUSE 挂载。请把 `/data/censorfs-demo` 放在本地 XFS 或 ext4 文件系统上。

```bash
export PATH="$PWD/target/release:$PATH"
export CENSORFS_DEMO_ROOT=/data/censorfs-demo
export CENSORFS_STORAGE_ROOT="$CENSORFS_DEMO_ROOT/.censorfs"
export CENSORFS_SOCKET="$CENSORFS_DEMO_ROOT/control.sock"

mkdir -p "$CENSORFS_DEMO_ROOT/import"
printf 'hello from base\n' >"$CENSORFS_DEMO_ROOT/import/hello.txt"

censorfs-init --import-root "$CENSORFS_DEMO_ROOT/import" --branch main
censorfs-daemon >"$CENSORFS_DEMO_ROOT/daemon.log" 2>&1 &
CENSORFS_DAEMON_PID=$!

for _ in $(seq 1 100); do
  [[ -S "$CENSORFS_SOCKET" ]] && break
  sleep 0.05
done

censorfs-info
censorfs-branch-create agent-a --from main
ticket=$(censorfs-explore agent-a --id-only)
censorfs-write --ticket "$ticket" /hello.txt --text 'hello from agent-a'
censorfs-cat --ticket "$ticket" /hello.txt
censorfs-commit "$ticket" --message 'agent-a result'
censorfs-head agent-a
```

`censorfs info` 依赖正在运行的 daemon。如果出现 `IoError: No such file or directory`，首先检查 `CENSORFS_SOCKET` 是否与 daemon 使用的路径一致，以及该 Socket 是否存在。完整启动顺序和每个命令的例子见 [CensorFS 命令使用手册](../../../../CensorFs/docs/CensorFS_CLI_GUIDE.md)。

测试结束后停止这个示例 daemon：

```bash
kill "$CENSORFS_DAEMON_PID"
wait "$CENSORFS_DAEMON_PID" 2>/dev/null || true
```

可读命令不创建 FUSE mount，因此不需要卸载 `/workspace`。

## 程序职责

| 程序 | 职责 |
|---|---|
| `censorfs` | 面向用户的多调用命令；`censorfs ls` 与 `censorfs-ls` 等价 |
| `censorfsd` | 独占持久化实例，负责恢复、状态机、控制协议和全部 FUSE Session |
| `censorfsctl` | 无特权底层调试客户端，通过 Unix Socket 调用 daemon |
| `censorfs-mounter` | 短生命周期特权助手，创建 Mount Namespace、挂载 `/workspace`、降权并执行 Agent |

三个底层程序共享 `censorfs-core`，不存在重复的文件系统实现。mounter 单独存在是为了隔离 Namespace 生命周期和特权边界；把它合入 daemon 会使 daemon 进入 Agent Namespace，扩大权限与故障影响范围。

## 代码结构

```text
.
├── api/censorfs.proto               # 长度前缀 Protobuf 控制协议
├── cmd/
│   ├── censorfs/                   # 可读多调用命令
│   ├── censorfsd/                   # 核心 daemon
│   ├── censorfsctl/                 # 底层控制客户端
│   └── censorfs-mounter/            # Namespace/FUSE 特权助手
├── crates/censorfs-core/src/
│   ├── model.rs, ids.rs            # 持久模型、状态枚举和强类型 ID
│   ├── codec.rs, persist.rs        # 磁盘编码、校验和原子持久化
│   ├── store.rs, branch.rs         # 对象存储、Ticket、发布、回滚和 Merge
│   ├── upper.rs, viewfs.rs         # Upper/Delta 与可测试 ViewEngine
│   ├── fuse_adapter.rs             # Linux FUSE 适配层
│   ├── control.rs                  # Unix Socket RPC、凭据和幂等请求
│   ├── namespace.rs                # Mount Namespace 与降权
│   ├── fsck.rs                     # 离线一致性检查与安全修复
│   └── fault.rs                    # 持久化边界故障注入
├── deploy/systemd/                 # systemd 服务示例
├── docs/                           # 架构、磁盘格式、命令和部署文档
├── scripts/                        # 构建、演示和真实 FUSE 测试
└── vendor/fuser/                   # 固定版本的 FUSE Rust 适配库
```

核心库按“磁盘与状态机”和“视图与 Linux 适配”分层。`ViewEngine` 不依赖真实 mount，可在单元/集成测试中覆盖版本和隔离语义；`fuse_adapter` 与 `namespace` 只负责把同一语义接入 Linux 数据面。

## 验证

核心测试：

```bash
mkdir -p /data/censorfs-test-tmp
TMPDIR=/data/censorfs-test-tmp \
  cargo +1.82.0 test --workspace --all-targets --locked
```

真实 FUSE 和 Mount Namespace smoke 测试需要 root、`/dev/fuse`、`jq`，并要求测试父目录位于本地 XFS/ext4：

```bash
sudo CENSORFS_TEST_PARENT=/data bash scripts/openeuler-real-smoke.sh
```

可读场景演示：

```bash
export PATH="$PWD/target/release:$PATH"
CENSORFS_DEMO_PARENT=/data bash scripts/demo-two-branch-isolation.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-merge.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-abort.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-crash-fsck.sh
```

## 文档

- [CensorFS 命令使用手册](../../../../CensorFs/docs/CensorFS_CLI_GUIDE.md)：初始化、daemon、分支、探索、提交、合并、fsck 和错误排查。
- [openEuler 鲲鹏构建与部署](../../../../CensorFs/docs/OPENEULER_AARCH64.md)：依赖、一键/手动/离线构建、运行前检查和真实 FUSE 验证。
- [架构说明](../../../../CensorFs/docs/ARCHITECTURE.md)：对象模型、发布、Merge、恢复、视图隔离和信任边界。
- [磁盘格式](../../../../CensorFs/docs/ON_DISK_FORMAT.md)：记录编码、目录布局、原子写入和兼容性规则。

## 生产边界

CensorFS 的故障注入测试覆盖系统调用持久化边界，但不能替代目标硬件上的物理断电、介质错误、写缓存和长时间 FUSE 压测。用于生产前，应在目标机型、目标内核和实际数据盘上完成这些验证，并为 `.censorfs` 配置备份、容量监控和访问控制。

MVP 不提供在线 Candidate GC。未采用 Candidate 会逻辑 Abort，但对象空间增长和 cgroup 残留必须纳入运维监控；DeepSeek Harness 的 `/censorfs-doctor` 是只读诊断入口，磁盘容量和 cgroup v2 委派/残留应由平台监控持续采集。
