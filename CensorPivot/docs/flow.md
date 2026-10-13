# CensorPivot 完整运行流程

## 1. 这条流程解决什么问题

CensorPivot 把一次 Agent 任务中的多个工具调用收成一个批次，并让它们同时获得三种能力：

- CensorFS 提供一份隔离、可丢弃、可一次性发布的文件工作区；
- CensorGuard 在可信 runner 接受用户工具前建立安全 domain，在工具运行时由 eBPF LSM 强制安全边界；
- CensorScope 从同一个真实进程 PID 开始观测整棵进程树，并把事件归属到具体工具调用。

Pivot 自己不重新实现文件系统、安全内核钩子或观测探针。它负责把三者按固定顺序接起来，
持久化唯一的 Commit/Abort 决定，再只沿这个决定推进第二阶段。

理解整条链路时，先区分两个时间段：

1. **部署与运行前准备：把边界装好。** 启动三个 daemon，安装 Guard 策略，初始化
   CensorFS 基线，确认 Scope 能采集。通常由管理员完成，不是每个批次都做一次。
2. **单批次运行：在既定边界内执行。** Pivot 开私有 View、启动受保护 runner、
   跟踪工具、冻结结果并 Commit 或 Abort。每个 Agent 批次都会走一遍。

### 当前适用范围

本文的单批次系统框图对应原生 Pivot `BatchRequest`（CLI、Demo 或 Unix socket 客户端）。
当前 DSH 聚合插件只是三个组件插件的安装和配置入口。普通对话仍走
`Web Guard attach_self -> Scope 常驻 worker -> 原生 DSH 工具`；CensorFS 的 View 由
分支探索等组件工具另行创建。普通 `read/bash` 尚未经过 Pivot ingress，不能认为自动
执行了图中的 `VariantOpen -> Prepare -> Commit/Abort`。真实核对记录见
[安装与编排验收](ACCEPTANCE.zh-CN.md)。

## 2. 一张完整可见的流程图

```text
部署准备（管理员，低频）

  /etc/agentcensor/censord.json（组合三个组件的启动配置）
    -> censord init                         [幂等初始化并校验三个组件]
    -> censord run                          [统一监督三个前台 daemon]
    -> censord doctor                       [检查三个真实控制接口]

  Guard policy YAML
    -> censorguardd 编译
    -> 写 inactive BPF policy bank
    -> 单次切换 active_bank
    -> /var/lib/censorguard/revisions/<revision>.yaml + current

  基线目录
    -> censorfsctl init
    -> CensorFS main Branch / 初始 Generation
    -> 启动 censorfsd，监听 /run/censorfs/control.sock

  /etc/censorscope/censorscoped.conf
    -> censorscoped init/start --level L3
    -> 监听 /run/censorscope/censorscoped.sock

  CensorPivot config
    -> 启动 censorpivot
    -> 监听 /run/censorpivot/control.sock


单批次运行（Pivot，每次请求）

  Agent Runtime
    -> BatchRequest

    -> CensorPivot 校验、幂等检查、持久化 Received/Preparing  //
    -> CensorFS VariantOpen                  [创建私有 Ticket + View，不发布]  //

    -> 启动 censorfs-mounter
         -> supervisor 创建批次 cgroup v2，worker 加入其中
         -> unshare 私有 Mount Namespace
         -> 在 /workspace 挂载 fuse.censorfs
         -> AttachFuse(View) 到 censorfsd
         -> 丢弃 root 权限与 capabilities
         -> exec CensorPivot __batch-runner
              -> runner 先 register_self 到 launch.sock
              -> Guard 用 SO_PEERCRED 取得 runner 的真实 PID
              -> 写入 domain/group 与 PID 的 BPF map
              -> runner 校验注册回执
              -> 回报 Ready(real host PID)，尚未接受或执行任何 Tool
    -> Scope track-add(real host PID)         [建立观测根]
    -> Tool 1..N 顺序执行
         -> Scope CallStart
         -> runner fork/exec Tool
         -> Guard 对真实 syscall 做 allow/deny [工具子孙继承 runner domain]
         -> /workspace I/O 经 VFS -> FUSE -> censorfsd -> Ticket 私有改动
         -> Scope 采集进程、文件、网络等事件
         -> Scope CallEnd
    -> runner 退出 -> mounter supervisor 清理全部后代并成功退出
    -> Scope TrackRemove

    -> CensorFS VariantPrepare               [冻结写入，形成 Candidate]  //
    -> Pivot 持久化唯一决定  //
         -> Commit: CensorFS VariantPublish CAS -> Branch Head 前进  //
         -> Abort:  CensorFS VariantAbort       -> 丢弃私有结果   //
```

这条链的核心不是“依次调用三个 API”，而是让同一个 runner 同时携带三种身份：

| 身份 | 由谁建立 | 作用 |
|---|---|---|
| 文件身份：Ticket/View | CensorFS `VariantOpen` + `AttachFuse` | 决定 `/workspace` 看到哪个基线、修改写到哪里 |
| 安全身份：domain/group | Pivot runner + Guard 私有 `register_self` | 决定这棵进程树允许哪些 file/exec/net 操作 |
| 观测身份：trace/session/call | Scope `track-add` + `CallStart/CallEnd` | 决定采集事件归属哪个批次和工具调用 |

## 3. 部署准备：先把三条数据面准备好

### 3.1 CensorGuard：先制定边界，再启动受保护进程

#### 3.1.1 策略写在哪里

Guard 的策略源是 YAML。单独部署 Guard 时的全量基础策略默认路径是：

```text
/etc/censorguard/base.yaml
```

使用 `scripts/install-agentcensor.sh` 统一部署时，`censord` 改用
`/etc/censorguard/agentcensor.yaml` 作为基础规则；安装器会在统一 daemon 就绪后，通过
`censorguardctl policy apply` 幂等下发 Pivot/DSH 使用的 `censorguard-dsh-default` 组。

Pivot 使用的策略组可以单独保存，例如：

```text
/etc/censorguard/policy.d/pivot-default.yaml
```

组策略文档只需要 `rules:`。下面是说明性示例，实际允许项应根据工具集合收紧：

```yaml
rules:
  - file deny+audit /var/lib/censorguard [write,delete,rename]
  - file deny+audit /run/censorguard [write,delete,rename]
  - exec deny+audit /usr/bin/rm -rf /
  - net deny 10.0.0.0/8
```

“文件放在 `/etc/censorguard`”只是方便管理员管理源配置，**并不代表策略已经生效**。
文件不会被内核自动读取。真正的生效路径是：

```text
YAML
  -> Guard Rust 编译器校验并规范化
  -> file/exec 目标尽可能解析为 inode，域名解析为网络地址
  -> 完整写入当前未激活的 BPF policy bank
  -> 一次 active_bank 切换
  -> 新策略开始被 eBPF LSM 使用
```

双 bank 的意义是避免热更新期间出现“半份旧规则、半份新规则”。新 bank 没写完整就不会
切换；切换后的持久化若失败，Guard 会尝试切回旧 bank。

#### 3.1.2 daemon 怎么启动、启动时做什么

生产环境由 systemd 启动：

```bash
sudo systemctl enable --now censorguardd.service
sudo systemctl status censorguardd
```

独立 Guard 的 production unit 等价于：

```bash
/usr/sbin/censorguardd launch --config /etc/censorguard/base.yaml
```

统一部署由 `censord` 启动：

```bash
/usr/sbin/censorguardd launch --config /etc/censorguard/agentcensor.yaml
```

不要同时启动独立 service。BPF 对象、socket、状态路径、gRPC、保护与审计开关都来自
YAML 的 `daemon`。启动时每次读取 YAML 的 `rules` 作为基础策略，再恢复已发布的动态组
与 domain 绑定；持久化快照不会替换基础规则或 daemon 开关。随后编译策略、挂载 hooks、
安装 BPF maps、创建 socket，健康后输出 READY。

`censorguardd reload --config PATH` 会完整重启并清空 PID 登记。统一模式应先退出 DSH，
通过安装器 `stop/start` 重启 Pivot 与数据面，再启动 DSH，避免运行中的工具失去登记。
仅调整动态组时使用 `policy apply`，无需重启。

#### 3.1.3 Pivot 策略组怎么安装

daemon READY 后，用控制接口安装组策略：

```bash
sudo censorguardctl --config /etc/censorguard/agentcensor.yaml policy apply \
  --policy pivot-default \
  --file /etc/censorguard/policy.d/pivot-default.yaml
```

该命令不是简单复制文件。它触发编译、inactive bank 写入、revision 快照持久化和
`active_bank` 原子切换。可以用以下命令确认实际生效状态：

```bash
sudo censorguardctl --config /etc/censorguard/agentcensor.yaml status
sudo censorguardctl --config /etc/censorguard/agentcensor.yaml status --json
```

`policy apply --policy` 的名称与 BatchRequest 的 `guard_group` 必须一致。
`register_self` 找不到组或无法绑定时，可信 runner 在输出 Ready 前退出，不接受工具。

该组还必须在 Pivot 管理员配置的 `censorguard.allowed_groups` 中。实际 domain 名由 Pivot
生成 `pivot-<transaction UUID>` 并落盘，不采用调用方传入的域名去复用现有安全域。

Pivot 不在打开 CensorFS View 前调用 `evaluate_intent`。真正启动 runner 时，Guard 根据
daemon 当前生效的 policy revision，将全局 baseline 与请求指定的 `guard_group` 组合成该
domain 的执行策略；它不是只用 `base.yaml` 做一次预过滤。策略绑定和 PID 跟踪成功前，
可信 runner 不会输出 Ready 或接受工具。

> Guard 的 file/exec 规则优先按 inode 编译。目标文件被替换而 inode 变化后，应重新
> apply/reload，不能只看路径字符串认为规则仍然命中。

### 3.2 CensorFS：建立基线并启动唯一存储进程

第一次部署先把一个普通目录导入成初始不可变 Generation，并让 `main` Branch 指向它：

```bash
sudo /usr/libexec/censorfs/censorfsctl init \
  --storage-root /var/lib/censorfs/.censorfs \
  --import-root <BASE_DIRECTORY> \
  --branch main
```

随后启动 daemon：

```bash
sudo systemctl enable --now censorfsd.service
sudo systemctl status censorfsd
```

production daemon 的核心参数是：

```text
/usr/libexec/censorfs/censorfsd
  --storage-root /var/lib/censorfs/.censorfs
  --socket /run/censorfs/control.sock
```

`censorfsd` 是该存储实例的唯一长期所有者，负责事务状态机、Journal、Object/Manifest、
Branch Head 和所有 FUSE Session。Pivot 只通过 control socket 发命令，不直接修改存储目录。

### 3.3 CensorScope：选择采集等级并启动观测面

```bash
sudo censorscoped init
sudo censorscoped start --level L3
censorscopectl doctor
```

默认 operator 配置位于 `/etc/censorscope/censorscoped.conf`。L1 提供进程与基本文件事件，
L2 增加 mmap、网络和 TLS，L3 再增加 IPC、stdout/stderr。Pivot 场景推荐 L3，具体等级可按
性能与审计需求调整。

Scope 是观测者，不做 allow/deny，也不决定文件事务是否提交。`track-add` 只是告诉 Scope：
“从这个 PID 开始，把它及其后代视为一棵需要归因的进程树。”

### 3.4 Pivot：检查四个服务是否可以连通

配置好 `CensorPivot/config.example.json` 中的组件路径、socket 和权限后启动 Pivot，再执行：

```bash
censorpivot doctor
```

Pivot 必须能访问 CensorFS control socket、执行受控 mounter、访问 Guard launch socket，
并调用 Scope control 接口。只有 Pivot 服务
账号拥有这些能力时，它才是“很薄，但正常部署下绕不过去”的必经接入层。

## 4. 单批次运行：逐步展开

### 4.1 接入、校验与幂等落盘

Agent Runtime 把 `BatchRequest` 发到 `/run/censorpivot/control.sock`。Pivot 通过
`SO_PEERCRED` 读取调用方真实 UID，校验工具路径、cwd、调用数量、环境变量和 Branch 的
预期 Head。

Pivot 先持久化 `Received`，再进入 `Preparing`。`request_id` 是幂等键：相同 ID 和相同
请求返回原事务；相同 ID 却内容不同则拒绝，防止一个名字代表两次不同执行。

### 4.2 CensorFS `VariantOpen`：开私有工作副本

接入校验和幂等落盘完成后，Pivot 直接调用 `VariantOpen`，携带：

- `branch`：从哪个公共分支开始，例如 `main`；
- `expected_generation` 与 `expected_head_seq`：调用方认为当前 Branch Head 是什么；
- `run_id`、`variant_id`：这次运行及方案的稳定标识。

先用几个日常概念理解返回对象：

| CensorFS 概念 | 通俗解释 | 类 Git 类比（仅帮助理解） |
|---|---|---|
| Branch Head | 公共可见版本指针 | `main` 当前指向的 commit |
| Generation | 一份不可变目录快照 | 一个 commit 的 tree 状态 |
| Tx | 事务记录 | 一次受控变更流程 |
| Ticket | 这次变更的操作凭证和私有改动容器 | 尚未提交的工作事务 |
| View | 给工具看到的“基线 + 本次私有改动”文件视图 | working tree，但由 CensorFS 隔离 |

`VariantOpen` 首先确认当前 Head 仍等于期望的 `(generation_id, head_seq)`，防止 Agent 基于
过期版本开始工作。检查通过后，它原子创建 Tx、Ticket 和可写 View，并返回 ticket ID、
view ID、owner UID/GID。

这一刻有三个重要的“还没有”：

1. **还没有挂载 `/workspace`。** `VariantOpen` 只建立服务端对象，实际挂载由 mounter 做。
2. **还没有改变 Branch Head。** 其他 Agent 仍然看到原公共版本。
3. **还没有发布任何修改。** 后续写入只在该 Ticket/View 内可见。

可以把它理解成“服务器确认基线没变，为本批次开了一间带编号的私人工作室”，但工具还没
走进工作室，公共展厅也没有变化。

### 4.3 `censorfs-mounter`：把 View 变成真实 `/workspace`

`censorfs-mounter` 是一个短生命周期、受控的特权 helper。普通 Agent 不应直接获得它的
执行权限，因为创建 mount namespace、打开 `/dev/fuse` 和挂载文件系统需要特权。

Pivot 启动它时传入 `view_id`、目标 UID/GID、CensorFS socket，以及下一条要执行的命令
`censorpivot __batch-runner --guard-socket ... --guard-domain ... --guard-group ...`。mounter 严格按下面的顺序工作：

1. `unshare(CLONE_NEWNS)`：为当前进程创建独立的 Mount Namespace。
2. 把 `/` 的 mount propagation 设为 recursive private，避免本 namespace 的挂载传播到宿主。
3. 创建 `/workspace`，打开 `/dev/fuse`。
4. 在本 namespace 的 `/workspace` 挂载 `fuse.censorfs`。
5. 调用 CensorFS `AttachFuse(view_id, owner_uid, owner_gid)`，通过 `SCM_RIGHTS` 把已打开的
   `/dev/fuse` 文件描述符交给 `censorfsd`。
6. 检查 `/proc/self/mountinfo`，确认 `/workspace` 确实已经挂载。
7. 清空补充组，切到目标 UID/GID，丢弃 capability bounding/effective/ambient sets，并设置
   `no_new_privs`。
8. `chdir("/workspace")`，用 `exec` 替换为可信 `censorpivot __batch-runner`。

上述步骤运行在 mounter 的 worker 中；Pivot 同时启用 mounter 已有的 cgroup supervisor。
supervisor 留在特权侧负责最终清理，worker 和工具进入批次 leaf cgroup。即使工具
`setsid` 脱离原进程组，仍能在批次结束时通过 cgroup 清理。Pivot 收到 supervisor 成功退出
后才执行 Prepare；挂载失败、注册失败或清理失败都会进入 Abort。

这里的 FUSE（Filesystem in Userspace）表示：工具仍然调用普通的 `open/read/write/rename`，
Linux VFS 发现 `/workspace` 是 FUSE 挂载后，把请求送到 `/dev/fuse`；持有另一端的
`censorfsd` 根据 `view_id` 读取基线或记录 Ticket 私有改动。工具不需要链接 CensorFS SDK。
VFS（Virtual File System，虚拟文件系统）是 Linux 在具体 ext4、XFS、FUSE 等文件系统之上的
统一入口；应用只面对同一套文件 API，VFS 再根据路径属于哪个挂载点把请求路由给对应实现。

Mount Namespace 隔离的是**挂载表**：runner 及其子进程继承这个 namespace，所以它们看到
私有 `/workspace`；宿主和另一个 Agent 的 namespace 看不到这次挂载。它不是完整容器，也
不是 chroot，不能天然隐藏所有宿主路径。`/workspace` 之外的访问仍需 Guard、容器或其他
sandbox 边界约束。

### 4.4 runner 为什么必须先注册，再接受工具

Pivot 自己属于可信控制面。mounter 建立 View 并降权后，exec 的仍是可信 Pivot runner；
用户工具尚未开始执行。runner 使用 Guard 共享 crate 的 v3 JSONL 协议：

```text
可信 __batch-runner 启动
  -> 连接共用 YAML 中配置的 launch.sock
  -> register_self(scope, group)
  -> daemon 用 SO_PEERCRED 读取真实 PID/UID
  -> 组合基础策略与指定动态组，安装 domain/PID 映射
  -> runner 验证版本、request ID、domain 和组回执
  -> 输出 Ready，开始接收 ToolCall
  -> fork/exec Tool，子孙继承 domain
```

连接失败、注册被拒、超时、超长响应或回执不匹配都会使 runner 退出。适配器还在
调用前后及批次结束时核查 daemon boot ID 与 runner 的 root/domain；发现登记丢失、
重启或失联便 Abort。它不能阻止重启窗口中的所有 syscall，所以维护仍必须统一 stop/start。即便 stdin 已提前
收到工具请求，也不会在注册成功前处理。这里保证的是用户工具在登记后执行；可信
runner 的装载和握手发生在登记前。旧独立 launcher 已移除，当前链路是：

```text
censorfs-mounter -> __batch-runner(register_self) -> Tool 1..N
```

### 4.5 runner 回报 PID，Scope 再开始跟踪

`__batch-runner` 完成 Guard 注册与回执校验后输出 `Ready { host_pid }`，然后阻塞等待 Pivot 从 stdin 发 ToolCall。
它不能在 Ready 之前执行 Tool。

Pivot 需要 runner 自己回报 PID，因为 Pivot 直接启动的子进程可能是 `sudo`、mounter 的
cgroup supervisor 或其他 wrapper，`Child::id()` 不一定是最终 runner。拿到真实 host PID 后，
Pivot 才调用 Scope：

```text
track-add(real_runner_pid, session_id, optional trace_id)
```

这样 Scope 建立的根与 Guard 注册的根是同一个进程。由于 runner 仍在等待，`track-add`
发生在第一个 Tool fork 之前，不会漏掉工具进程的起点。

### 4.6 Tool 循环：三个组件如何同时工作

以一个工具写 `/workspace/result.txt` 为例：

```text
Pivot -> Scope CallStart(call_id)
Pivot -> runner ToolCall(/usr/bin/sh -c "printf ok > /workspace/result.txt")
runner -> fork/exec /usr/bin/sh

  1. execve("/usr/bin/sh", ...)
       -> CensorGuard eBPF LSM 检查 runner 所属 group 的 exec 规则
       -> CensorScope 记录进程/exec 事件

  2. openat("/workspace/result.txt", O_WRONLY|O_CREAT, ...)
       -> CensorGuard eBPF LSM 检查 file write 规则；拒绝则返回 EPERM
       -> Linux VFS 识别该路径属于 fuse.censorfs
       -> FUSE 请求送到 censorfsd
       -> censorfsd 把新内容写入本 Ticket 的 Upper/Delta
       -> CensorScope 记录文件事件并归到当前 trace/call

  3. 工具退出
       -> runner 把 exit code/stdout/stderr 回给 Pivot
       -> Pivot -> Scope CallEnd(call_id, outcome)
```

后续 Tool 在同一个 runner、同一个 mount namespace 和同一个 View 中执行，所以 Tool 2 能看到
Tool 1 写入的文件。工具按顺序执行；任一个返回非零或组件发生 required 级错误，Pivot 停止
后续调用并走 Abort 决定。

Guard 和 Scope 都可能使用 eBPF，但职责不同：Guard 在 LSM hook 上同步返回 allow 或
`-EPERM`，是执行路径上的强制者；Scope 从 tracepoint/uprobe/procfs 等采集事实，是旁路
观察者。Guard 的 domain 绑定“这棵进程树受哪套规则”，Scope 的 trace root 绑定“这棵进程
树产生的事件记到哪里”。

### 4.7 `VariantPrepare`：冻结结果，但仍未公开

所有 Tool 成功后，Pivot 关闭 runner，等待 mounter supervisor 清理后代并成功退出，
调用 Scope `TrackRemove`，再调用 CensorFS
`VariantPrepare(ticket_id)`。Prepare 会禁止该 Ticket 继续写入，并根据私有 Delta 生成不可变
Object、Manifest、Generation 和 Candidate。

Candidate 可以理解成“已经封箱并编号、等待是否上架的版本”。它证明文件结果已经固定，
但 Branch Head 还没有改变。此后 Pivot 将事务状态写为 `Prepared`。

### 4.8 决策先落盘，第二阶段后执行

到 `Prepared` 后，Pivot 决定 Commit；此前任何失败则决定 Abort。决定和状态通过临时文件、
`fsync(file)`、`rename`、`fsync(directory)` 持久化。只有决定可靠落盘后才调用 CensorFS：

```text
decision = commit -> 以后只能重试 VariantPublish
decision = abort  -> 以后只能重试 VariantAbort
```

即使 Pivot 在决定落盘后崩溃，恢复程序也不会重新执行工具或把 Commit 翻成 Abort。

### 4.9 `VariantPublish` CAS：只在起点没变时发布

CAS 是 Compare And Swap。假设批次开始时：

```text
期望 Branch Head: (G10, head_seq=5)
本批次 Candidate: G11
```

Publish 只在当前 Head 仍然是 `(G10, 5)` 时成功：

```text
仍是 (G10, 5) -> 原子推进为 (G11, 6)
已经被别人推进 -> HeadChanged，不覆盖对方结果
```

这不是单纯借用 CPU CAS 指令，而是 CensorFS 在 Branch 锁、Journal、A/B Head slots、fsync 和
receipt 基础上提供的持久化逻辑 CAS。它解决两个 Agent 从同一基线并发完成时的丢失更新问题。

Abort 则丢弃 Ticket 的私有结果，不移动 Branch Head。无论哪条分支完成，Pivot 最终记录
`Committed` 或 `Aborted`。

## 5. 失败时会发生什么

| 失败位置 | Pivot 结果 | 原因 |
|---|---|---|
| 接入校验失败 | 不打开 View，直接返回错误 | 请求结构或接入参数不合法 |
| `VariantOpen` Head 不匹配 | Abort/冲突返回 | 不在过期基线上静默执行 |
| mounter/FUSE/AttachFuse 失败 | Abort | 工具没有可靠的私有 `/workspace` |
| Guard `register_self` 失败 | Abort，runner 不输出 Ready、不接受工具 | 内建注册门禁保持 fail-closed |
| Scope `track-add` 失败 | required=true 时 Abort；否则 warning | 观测策略可配置，但不削弱 Guard/CensorFS |
| 任一 Tool 返回非零或超时 | 停止后续 Tool，Abort | 整批文件修改不发布 |
| `VariantPrepare` 失败 | Abort | 没有得到不可变 Candidate |
| Commit 决定落盘后 Publish 暂时失败 | 保持 CommitDecided，只重试 Publish | 决定不可翻转 |
| Publish CAS 冲突 | 保持可见冲突，不覆盖新 Head | 并发者已经改变公共分支 |
| Abort 决定落盘后 Abort 暂时失败 | 保持 AbortDecided，只重试 Abort | 决定不可翻转 |

Guard 拒绝的是具体 syscall。工具吞掉 EPERM 后仍返回 0 时，Pivot 不会仅凭审计事件决定 Abort。

`Preparing` 阶段崩溃后，Pivot 不重放可能已经发生外部副作用的 Tool，而是决定 Abort。文件
结果可以丢弃，但已经发出的网络请求、数据库写入或消息无法由 CensorFS 回滚。因此 Pivot 的
事务承诺覆盖 CensorFS 文件结果与自身决策，不应被描述成任意外部世界的分布式事务。

## 6. 运维验收清单

在把 Agent 流量接入 Pivot 前，至少确认：

- `censorguardd` 日志出现 `[READY]`，`status` 和 `status --json` 显示预期 revision/group；
- Pivot 请求中的 `guard_group` 与 `policy apply --policy` 完全一致；
- CensorFS 已完成 init，`censorfsd` 持有实例并监听 control socket；
- `censorfs-mounter` 只能由受控的 Pivot 路径提权执行，Agent 用户不能随意调用；
- CensorScope daemon 已按预期等级运行，`censorscopectl doctor` 正常；
- `censorpivot doctor` 检查命令/socket、FUSE/cgroup 环境并验证 FS/Guard/Scope 往返；
- 实测一个允许操作能 Commit，一个 Guard 拒绝操作会整批 Abort；
- 实测两个请求基于同一 Head 时最多一个 CAS Publish 成功；
- 重启 Pivot 后，`CommitDecided`/`AbortDecided` 只继续原决定，不重新执行 Tool。

相关的接口字段、系统分层与状态机定义见 [设计文档](DESIGN.zh-CN.md)；三个组件的功能边界
与仓库实现位置见 [组件功能与接口说明](COMPONENTS.zh-CN.md)。
