# CensorScope 接入修复交接说明（2026-10-13）

本次修改覆盖 Scope daemon 的进程授权、采集到存储的收尾、重启后的去重、DSH
session proxy 的模块入口和会话配置同步，以及 Guard 保护下的 trace 登记。
下面按同事审查时需要关注的边界说明。

## 1. 私有 mount namespace 中的 runner 登记

文件：`CensorScope/crates/apps/daemon/src/peer_identity.rs`。

原生 Pivot 以非 root 用户运行，CensorFS mounter 为 runner 创建私有 mount namespace。
Pivot 启动的 Scope ctl 与 runner 同 UID、同 PID namespace，但不同 mount namespace。
旧 `authorize_process_ref` 拒绝了该目标，结果是工具实际运行，却无法建立 Scope trace。

现在保留原来的 principal 匹配路径，增加一个限定的跨 mount namespace 委托路径：

- 控制 socket peer 必须非 root；peer、coordinator、target 必须同 UID。
- coordinator 是 ctl peer 的直接父进程；coordinator 与 peer 的 PID/mount namespace
  principal 必须匹配，target 的 PID namespace 必须与 peer 相同。
- 沿 target 的 `/proc/PID/stat` 父链向上查找 coordinator，最多 256 层；不属于该树则拒绝。
- proc stat 解析使用最后的 `)` 作为 comm 结束标记，兼容带空格和括号的进程名。

这是安全授权语义的新增分支，同事应重点审查其父链、UID、namespace 检查。
`is_trusted_host_root`、原有同 namespace 规则和 trace owner 权限检查没有被替换。
新增测试覆盖合法后代、无关进程、跨 UID/PID namespace、错误 coordinator namespace
以及 root peer 不可使用新增委托路径。

## 2. 短批次结束前等待采集事件提交

文件：`crates/apps/daemon/src/collector_actor.rs`、`service_host.rs`。

工具执行只需几毫秒时，事件可能仍在内核 transport、spool 或 writer 队列。旧
track-remove 在这些事件完成解码/落盘前忘记 trace，导致短批次只留下 call spans，
没有内核事件或无法保留其归属。

现在 Unbind 会先 flush/poll 原始事件，再解除绑定、再次排空、同步 spool，并返回
`next_sequence` watermark。service host 在忘记 trace 前继续解码和提交，直到数据库
确认处理到该 watermark；最多等待 10 秒，超时报 `trace_remove_drain`。

接口变化需要同步审查：

| 内部接口 | 变化 |
|---|---|
| `CollectorBatchSink` | 新增 `next_sequence()`，默认 0，实际 spool sink 返回下一序号 |
| `CollectorActorHandle::unbind_trace` | 从 `Result<(), CollectorError>` 改为 `Result<u64, CollectorError>` |
| `DaemonServiceHost` | 记录已提交的 spool 下一序号，track-remove 返回前执行排空屏障 |

该屏障目前用于显式 track-remove。trace re-root 和 daemon shutdown 的现有 Unbind
调用只忽略返回的 watermark，本次没有将同样的等待逻辑扩展到这两条路径。

## 3. daemon 重启后，新事件被误认为旧批次

文件：`crates/storage/core/src/backend.rs`、
`crates/storage/adapters/sqlite/src/{backend,lib}.rs`、`service_host.rs`。

原实现启动时删除旧的 daemon-lifetime spool，使新 spool 从序号 0 开始；但 SQLite 的
`spool_checkpoint.last_sequence` 保留上次运行的值。writer 将新序号当成已提交批次跳过，
表现为 daemon、trace、call spans 正常，新内核事件却不落盘，直到序号超过旧 checkpoint。

现在在删除旧 spool 后、启动 collector/writer 前重置该 checkpoint 为 -1。
`StorageBackend` 新增 `reset_spool_checkpoint()`，SQLite 实现该方法。历史 traces、events、
processes 等数据仍保留，同一新 spool 内重复提交同一序号仍然幂等。没有修改数据库 schema。

存储回归测试验证：旧批次只写一次；重置 checkpoint 后，新 spool 的序号 0 被接受；
再次提交序号 0 不重复写入。此修复沿用“spool 只属于一次 daemon 生命周期”的既有设计，
没有增加跨重启 spool 恢复功能。

## 4. DSH session proxy 修复

文件：`plugins/agentcensor-session-proxy/lib/{index,interaction-bridge,worker}.mjs`，
以及该插件的 `README.md`。

**模块身份：** 旧代码只识别 `tsx/esm`，没有识别解析后的绝对路径
`tsx/dist/esm/index.mjs`，会同时加载 DSH 源码与构建产物的 scope 模块，产生不同的私有
Symbol。实际错误为 `operation requires the Agent's own scope`，会话 worker 无法启动。
现在两处 import 使用相同入口检测，让源码 CLI 与插件加载同一份 scope 模块。

**常驻 worker 配置：** 浏览器修改权限时，宿主 session 已更新，但 worker 继续使用旧
权限。现在同步四类原生配置事件：`permission/preset`、`sandbox/mode`、`approval/policy`、
`model/selection`。worker 经原生 session fold 应用，避免另行复制权限判断逻辑。

**事件游标：** host 的命令/标题事件会使 host 和 worker 序号不同。现在用独立的
`workerNextSeq` 检查 worker 连续性；`host-session-event` 传配置、`ACSYNC` 确认同步。
两端用序号集合识别已转发事件，防止异步 event 回声和重复写入。
宿主仍是共享会话日志的持久化写者，由 worker 生成的工具/审批内容保持原生事件语义。
插件说明同步更新了协议与事件所有权。

这部分改动位于 Scope 仓库的 DSH 插件，属于会话运行适配，不是 eBPF 规则或探针修改。

## 5. Guard 拦截 Scope：组权限与内核保护的区别

文件：`CensorScope/crates/apps/ctl/src/args.rs`、
`CensorScope/plugins/censorscope-host/lib/index.mjs`、该插件的 `package.json`。

同事提出的 `sudo usermod -aG censorguard alice` 只可能解决 Guard socket 的文件权限，
不能豁免受管进程树的内核检查。尚未拿到同事当时的命令与错误，不能断言当时只有
哪一种原因；本次隔离测试确认以下三类问题可以分别发生：

| 层次 | 典型表现 | 原因与处理 |
|---|---|---|
| Unix socket 文件权限 | 连接 Guard socket 报 Permission denied，没有对应的 Guard 拒绝事件 | 核查所有上级目录、socket 实际属组和当前进程组；按实际属组配置访问权限 |
| Guard 内核保护 | 读取 `/proc/PID/ns/pid` 失败；审计显示 `GUARD ptrace DENY`，或加载探针显示 `GUARD bpf DENY` | ctl 改为传明确的 PID namespace；Scope daemon 独立运行在 Guard 受管树外 |
| Scope 控制面授权 | 已连到 Scope，但返回 `cannot act for target` 等错误 | 核查 socket peer 与目标的 UID、namespace、父链和 trace owner；加入 Guard 组不会改变这些条件 |

**实际复现的登记冲突：** DSH host 在 Guard 受管树内创建 ctl 子进程，调用
`track-add --root-pid <DSH PID>`。旧 ctl 读取宿主的 `/proc/PID/ns/pid`，内核将这一访问
交给 `ptrace_access_check`。因此即使没有使用调试器、两进程同 UID、调用者是 root，
仍触发 Guard 对受管进程的 ptrace 限制，报 `Permission denied (os error 13)`。
Guard 审计记录为 `kind=4, op=2, allowed=false`。

现在 ctl 增加可选参数 `--root-pid-namespace 'pid:[INODE]'`，使用现有 `ProcessRef`
传递该 namespace 内的 PID，无须在客户端读取目标 procfs。DSH host 自己读取
`/proc/self/ns/pid`，与 `process.pid` 一起传给 ctl；插件升级到 **0.1.2**。
未指定新参数时保留原有行为，控制协议不变。daemon 仍解析目标并执行既有授权，
没有因为参数来自客户端而信任目标，也没有增加 UID、组或程序名豁免。

**Scope daemon 的边界：** 不能从 Guard 管理的 shell/DSH worker 中启动 Scope daemon。
它需要读取其他进程的身份、调用 `bpf()` 装载采集器；受管树内的这些操作会被拒绝。
`censord` 已将 Scope 作为独立组件启动，DSH 插件仅连接控制面。Scope 的采集器有
延迟初始化路径，`doctor` 可达不等于 `track-add` 成功，更不等于已有真实事件落盘。
本次另用实际 BPF map-create 验证：树外 root 成功，树内 root 即使带 `censorguard`
附加组仍报 EPERM；无需放宽 Guard 的 ptrace/BPF 防护。

**选择正确的 OS 组：** 独立 Guard 示例配置的 `daemon.socket_group` 是 `censorguard`；
AgentCensor 统一部署使用 `censorpivot`，默认 `/run/censorguard` 目录也归该组。
应以运行配置和目录/socket 实际属组为准。配置变更或保留旧配置可能导致两者不同，
仅修改用户组不能解决这种不一致。加组后还需要重新登录或重启相关进程；`id alice`
显示账户组，DSH 用户当前终端里的 `id` 才反映该进程实际继承的组。

排查示例（socket 路径和配置路径按部署调整）：

```bash
id
id alice
namei -l /run/censorguard/dsh.sock
stat -c '%A %U:%G %n' /run/censorguard /run/censorguard/*.sock
sudo censorguardctl --config /etc/censorguard/agentcensor.yaml \
  audit --kind guard --result deny
```

在另一终端重现错误。上述审计是实时订阅，应先开启再执行测试。若出现 `ptrace` 或
`bpf` 拒绝，继续加组无效。Scope 自己的 socket 默认 `0666`，权限由 daemon 根据
`SO_PEERCRED` 校验；应结合 `censorscopectl --json doctor` 和实际 track-add 的回复诊断。

同步更新步骤（仓库根目录执行，先退出 DSH；以实际运行 DSH 的用户安装插件）：

```bash
sudo -H bash CensorPivot/scripts/install-agentcensor.sh stop
sudo -H bash CensorPivot/scripts/install-agentcensor.sh scope
sudo -H bash CensorPivot/scripts/install-agentcensor.sh start
DSH_ROOT=/root/deepseek-harness bash CensorPivot/scripts/install-dsh-censorpivot.sh
```

需要更新 `censorscopectl` 和 host 插件；单独安装 0.1.2 插件配旧 ctl 会报未知参数。
Scope daemon 本次控制协议保持兼容，重新安装仍应包含前面几项 daemon/storage 修复。

## 6. 验证与提交范围

上次最终验证：daemon 58 个库测试、5 个二进制测试、SQLite 24 个测试通过。
跨 namespace 委托测试和重启去重回归包含在其中。真实 FS/Guard/Scope/Pivot 批次证明
每个执行过的工具都有内核事件和 call_id；最终 Chromium 测试覆盖 worker 已创建后切换
权限、读取/删除拒绝、后续对话和无序号断档。

本次权限排查补充验证：ctl 16 个测试、daemon 58 个库测试及 5 个二进制测试、SQLite
24 个测试通过；4 个可选性能测试未运行。host 登记 Node 回归通过。
新增真实 Guard/Scope 回归脚本：

```bash
sudo -H python3 CensorScope/scripts/test-guard-compatibility.py
node --test CensorScope/plugins/censorscope-host/test/host-registration.test.mjs
```

真实回归需要先构建两个组件的 release 二进制和 Guard BPF object，要求 root、BPF LSM、
tracefs、Node 22+、setpriv 和已有 `censorguard` OS 组。脚本只使用临时配置、socket 和数据库，
不修改账户或已安装服务，证据留在 `/tmp/scope-guard-audit-*`。
已验证旧路径失败、新路径在 root/普通 UID 下成功（加组和不加组均可），真实 host 模块
在 Guard 树内登记并留下内核事件；不存在的 namespace 和跨 UID 目标仍被拒绝。
同时验证 socket 访问受 OS 组影响，组成员不因此拥有 Guard 策略管理权。

上次验收证据及环境限定见 [验收报告](ACCEPTANCE.zh-CN.md)。Scope 的既有 Clippy 告警仍未
全部消除；本次 ctl 的严格 Clippy 检查也因 8 项既有告警失败（`ptr_arg`、`collapsible_if`、
`too_many_arguments`）。不能将测试通过理解为整个仓库 lint 全通过。

上述修改没有编辑 Scope 的 eBPF C 探针、采集等级定义、事件结构或数据库 schema。
需要同步合并的内部 Rust 接口是 Unbind watermark、sink next_sequence 和 storage
checkpoint reset；DSH 宿主与 worker 应一起重新打包安装，避免新旧协议混用。
新增 ctl 参数及 host 0.1.2 同样需要同步交付；组合包中的 `censorscope-host.tgz` 和校验值
已更新。

建议将 Scope 文件独立提交，便于维护者审查和 cherry-pick：

```text
fix(scope): preserve batch events and synchronize DSH worker state

- Allow same-UID coordinator descendants in private mount namespaces
- Drain collected events before removing trace attribution
- Reset the database replay checkpoint for each new daemon spool
- Align DSH scope imports and sync resident worker configuration
- Cover authorization boundaries, restart deduplication and runtime behavior
```
