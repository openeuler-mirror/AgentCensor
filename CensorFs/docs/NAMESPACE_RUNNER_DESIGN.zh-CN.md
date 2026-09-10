# Namespace Runner 生产隔离设计（zh-CN）

> 范围：DeepSeek Harness 集成中 `branch_explore_inprocess` 的 Runner 隔离能力，覆盖配置模型（`runnerIsolation`）、生产自动探测（`probeRunnerEnvironment` / `detectRunnerEnvironment`）、安全 fallback、`/censorfs-doctor` 诊断、实际隔离报告、旧 `runnerCgroup`/`childCommand` 兼容，以及生产与 TEST-ONLY E2E 的安全边界。
> 对应实现：`integrations/deepseek-harness/src/runner-environment.js`（探测与归一化）、`src/index.js`（启动接线与 doctor 命令）、`src/runtime.js`（exploration 事件快照与 doctor 入口）、`src/runner-manager.js`（探测结果驱动 cgroup 开关）、`cmd/censorfs-mounter/src/cgroup.rs`，以及 `scripts/openeuler-*-namespace-runner-*.sh` smoke 脚本与 TEST-ONLY 的 `scripts/openeuler-dsh-inprocess-e2e.sh`。

---

## 1. 背景：Runner 隔离的三个层次

进程内 Runner（`branch_explore_inprocess`）把 Agent 的模型上下文留在 Host 进程内，为每个 child Agent 启动一个专属 Namespace Runner：`censorfs-mounter` 创建独立 Mount Namespace 并在其中挂载真实 `/workspace`，随后 `exec` 插件自带的 `runner-process.js`。隔离能力按强度分为三个可测层次：

| 层次 | 名称 | 保证 | 探测依据 |
|---|---|---|---|
| `process` | 进程级 | 独立进程 + 专属 Mount Namespace + 降权用户（mounter 清空补充组/能力后切换 Agent UID/GID）；文件操作只落在该 View 的真实 `/workspace`；`bash` 一律经 bubblewrap 最小 rootfs | mounter 可执行、`/dev/fuse` 可用、Node 可执行 |
| `lifecycle` | 生命周期级 | 在 `process` 之上，worker 及全部后代被放入独立 cgroup v2 scope；`cgroup.kill → populated=0 → rmdir` 可回收正常退出、Host 死亡和 stale scope；supervisor 留在 scope 外 | 已委派的 cgroup v2 根目录及其结构文件（`cgroup.controllers/procs/type/events/subtree_control`）存在且可读（advisory，见 3.3） |
| `resource` | 资源限额级 | 在 `lifecycle` 之上，按配置写入 `memory.max`（含 `memory.oom.group`）、`pids.max`、`cpu.max` 限额 | `memoryMax`/`pidsMax`/`cpuMax` **三项全部配置** 且 `memory`/`pids`/`cpu` 三个 controller 均可用（advisory） |

`LEVELS = { process: 0, lifecycle: 1, resource: 2 }`，`minimumLevel` 只接受这三个值，比较按该序进行；`external` 模式不进入该序（见 2.2）。

## 2. 配置模型：`runnerIsolation`

`runnerIsolation` 是 Runner 隔离配置对象，取代旧 `runnerCgroup` 的决策位置；旧字段继续作为兼容来源（见第 7 节）。归一化由 `normalizeIsolationConfig(value, legacy)` 完成，启动时在 `normalizeConfig` 中强制调用，配置非法直接抛 `TypeError`（fail fast，不静默修正）。

### 2.1 字段

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `mode` | `auto` \| `required` \| `process` \| `external` | `auto`（旧 `runnerCgroup.enabled: true` 时为 `required`） | 隔离意图与降级许可，见 2.2 |
| `minimumLevel` | `process` \| `lifecycle` \| `resource` | `process`（旧 `runnerCgroup.enabled: true` 时为 `lifecycle`） | 必须达到的最低实际层次，见 2.3 |
| `root` | string（绝对路径） | 无 | 已委派给 CensorFS 的 cgroup v2 空子树 |
| `stateDir` | string（绝对路径） | 无 | cgroup 状态与 marker 目录；`root` 提供时**必填** |
| `memoryMax` / `pidsMax` / `cpuMax` | string | 无 | 直接写入对应 cgroup v2 文件的限额（如 `2147483648`、`256`、`'200000 100000'`） |
| `cleanupTimeoutMs` | 正整数 | `5000` | stale scope 回收等待上限 |

### 2.2 `mode` 语义（与探测行为一一对应）

- **`auto`（默认）**：先探测；cgroup v2 委派可用就提升到探测到的层次并启用 cgroup，不可用则回落到 `process`。**永不抛错**：包括低于 `minimumLevel` 的情形也只记录 warning（`effective isolation <level> is below minimumLevel <level>; auto mode continues at <level>`），随后继续运行。生产环境若只求“有更好，没有也能跑”，用 `auto`。
- **`required`**：实际层次低于 `minimumLevel` 即 fail closed。注意：`minimumLevel: process` 时由 Mount Namespace 底线本身即可满足，**不强制 cgroup v2**；只有把 `minimumLevel` 提到 `lifecycle`/`resource` 才强制 cgroup 委派。旧 `runnerCgroup.enabled: true` 等价 `required + lifecycle`，因此继续强制 cgroup，与旧语义一致。
- **`process`**：明确只要进程级隔离，即使 cgroup v2 完全可用也不启用；此时若 `minimumLevel` 高于 `process`，模式自身无法提供而 fail closed。
- **`external`**：隔离由外部边界（CensorFS/mounter 边界、容器、VM、pod 安全策略）负责，本插件不管理 cgroup v2 也不强制 `minimumLevel`。**`external` 是部署声明，不是隔离等级**：本地可观察等级仍如实报告为 `process`（`effectiveLevel: 'process'`、`cgroupEnabled: false`），绝不把外部声明伪装成 `effectiveLevel`。探测记录 warning：`Runner isolation is delegated to an external boundary; locally observed isolation remains process`。

### 2.3 `minimumLevel` 语义

- 只允许 `process` / `lifecycle` / `resource` 三个值（`external` 模式不适用）。
- fail-closed 判定（`probeRunnerEnvironment` 内）：`LEVELS[effectiveLevel] < LEVELS[minimumLevel]` 且 `mode !== 'auto'` 且 `mode !== 'external'` 时产生 `error` 字段（`required`/`process` 会拒绝启动）。
- `auto` 下显式设置 `lifecycle`/`resource` 的语义：探测不到就记录 warning 并继续以低层次运行——不会失败，因此生产如需“探测不到宁可失败”应使用 `required`。

## 3. 生产自动探测

### 3.1 两个入口

| 函数 | 行为 | 使用方 |
|---|---|---|
| `probeRunnerEnvironment(config, env)` | **只读、永不抛错**；返回完整报告，策略失败时在 `error` 字段携带原因 | 插件启动（`apply`）与 `/censorfs-doctor`（`runtime.doctor()` 直接复用） |
| `detectRunnerEnvironment(config, env)` | 包装前者：报告含 `error` 时抛出该错误 | `runtime.exploreWithMode` 的 in-process 路径在每次探索前 fresh 执行并 fail closed |

探测全程只 `stat`/`access`/`connect`：不启动 daemon、不写 sudoers、不挂载、不创建 cgroup，无任何副作用。

### 3.2 探测项

`probeRunnerEnvironment` 并行执行七项只读探测：

| 探测项 | 判定 | 失败影响 |
|---|---|---|
| daemon socket（`probeDaemonSocket`） | 对 `config.socket` `stat` 后 `connect` 立即关闭；区分 live / stale socket（存在但无监听）/ 缺失 | 缺失或 stale 记入 warning（`daemon socket not found` / `socket exists but no daemon is listening`） |
| `censorfsCommand` 可执行 | 按 `PATH` 或绝对路径 `access(X_OK)` | warning：`censorfs command is not executable` |
| `mounterCommand` 可执行 | 同上 | warning：`mounter command is not executable`；Runner 无法启动 |
| `process.execPath`（当前 Node）可执行 | `access(X_OK)` | warning |
| `bwrap` 可执行 | 按 `PATH` 查找 | warning：`bwrap is unavailable; runner bash tools will fail closed` |
| `/dev/fuse` 是字符设备 | `stat` 检查 | warning：`/dev/fuse is unavailable` |
| cgroup v2 能力（`cgroupCapability`） | 只读结构验证（见 3.3），结果为 advisory | 决定 `effectiveLevel` 与 `cgroupEnabled`；真实可写性由 mounter readiness 权威判定 |

### 3.3 `cgroupCapability` 探测逻辑

1. `root` 未配置 → `{ available: false, level: 'process', reason: 'no delegated cgroup v2 root configured' }`。
2. `stat(root)` 必须是目录；`cgroup.controllers`、`cgroup.procs`、`cgroup.type`、`cgroup.events`、`cgroup.subtree_control` 必须存在且可读（仅 `access(R_OK)`，绝不写入）——确认这是真实 v2 委派子树而不是普通目录。**不用 Host 身份的可写性做否决**：生产 mounter 可能特权而 Host 非 root，Host `W_OK` 缺失不代表委派无效；真实可写性由 mounter launch/readiness 握手权威判定（`required` fail closed、`auto` 运行期降级到 process）。任一失败 → `available: false, level: 'process'`，reason 记录原始错误；可用时返回 `advisory: true`。
3. 限额与 controller 的对应：`memoryMax` → `memory`，`pidsMax` → `pids`，`cpuMax` → `cpu`；配置了限额但 controller 不在 `cgroup.controllers` 中 → 记入 `missingRequestedControllers`。
4. 层次判定（按用户契约，**resource 必须是三件套齐全**）：
   - `memoryMax`、`pidsMax`、`cpuMax` **三项全部配置** 且 `memory`/`pids`/`cpu` **三个 controller 均可用** → `level: 'resource'`；
   - 否则只要根可用 → `level: 'lifecycle'`。告警门槛（避免无意义告警）：`missingResourceLimits` warning（`resource isolation requires memoryMax, pidsMax, and cpuMax; missing: <list>`）**仅当 `minimumLevel=resource` 或至少配置了任一限额但不完整时**产生——默认 lifecycle 且完全不配限额不产生该告警；配置了但 controller 缺失的走 `missingRequestedControllers` warning（`requested cgroup controllers unavailable: <list>`）。
5. 任何异常统一降级为 `available: false, level: 'process'`，reason 带错误文本——**探测失败不当作 cgroup 可用**。

### 3.4 探测结果如何生效（不只是显示）

- **启动期**：`apply` 在注册任何工具/命令前运行 `probeRunnerEnvironment`（只读、永不抛错）；把结果写入 `config.runnerIsolation.cgroupEnabled` 与 `config.runnerIsolation.environment`，并逐条记录 info/warning（含 daemon live/stale、controllers、fuse、bwrap）。启动探测失败不阻止插件加载，但会记录 error——真正的 fail-closed 发生在 **exploration 执行前**（见下）。
- **exploration 执行前（in-process）**：`runtime.exploreWithMode` 在每次 `branch_explore_inprocess` 前 fresh 运行 `detectRunnerEnvironment`：策略 fail-closed 直接抛错；同时要求 live daemon socket、censorfs/mounter/node/bwrap 可执行、`/dev/fuse` 可用，任一缺失即拒绝并提示运行 `/censorfs-doctor`。外部 `branch_explore` 不经过该隔离门禁（只做 `inProcessOnly`/`childCommand` 校验）。
- **Runner 启动时**：`RunnerManager.launch` 依据**探测结果** `runnerIsolation.cgroupEnabled === true`（而非配置原文 `runnerCgroup.enabled`）决定是否传 `--cgroup-*` 参数——生产实际运行的隔离正是探测认定的隔离，doctor 与运行路径共用同一判定。
- **`doctor` 期**：`runtime.doctor()` 重跑 `probeRunnerEnvironment` 并原样保留完整报告（含 fail-closed 时的 `error` 与全部诊断细节），见第 5 节。
- **auto 运行期安全 fallback（launch 失败）**：启动探测选择了 cgroup，但某 Runner 的 launch/readiness 失败时（如委派子树在运行时不可用），`auto` 模式清理失败 Runner（`RunnerManager.launch` 既有 `disposeRecord` 路径），把共享的 `runnerIsolation.cgroupEnabled` 置 false、`environment.effectiveLevel` 降为 `process` 并追加 warning，写入 `isolation-fallback` Session 事件与 harness 日志，然后**以 process 重试一次**。事件携带 `fromLevel`/`originalFromLevel`（本次尝试实际从何级发起，捕获于首次 launch 前，不受并行 variant 降级影响）、`toLevel`、warning、时间戳；fold/客户端同步把**运行级 `isolation` 降为 `toLevel`**（`effectiveLevel`、`cgroupEnabled: false`、删除 `cgroupRoot`、追加 warning），并记入 variant 的 `isolationFallback` 与运行级 `isolationFallbacks` 列表，因此 UI/header 不再永久显示启动时的 resource。fallback 判定依据**本次尝试**捕获的 `attemptedCgroup`（而非共享 `cgroupEnabled`，另一个并行 variant 可能已先置 false），因此每个确实尝试过 cgroup 且失败的并行 variant 都各自以 process 重试一次。`required` 不重试，保持 fail closed。降级后 guard 不再满足，故重试至多一次、无无限循环；失败 Runner 由 launch 清理路径回收，不产生双重残留。

## 4. 安全 fallback

总原则：**探测不确定时降级，降级不满足要求时失败，失败绝不静默落到 Host 文件系统或未隔离进程**。

### 4.1 mode × 探测结果的行为

| mode | cgroup 可用 | 无 cgroup 根 | 仅部分 controller | 低于 `minimumLevel` | `bwrap`/fuse 缺失 |
|---|---|---|---|---|---|
| `auto`（默认） | 提升到 `lifecycle`/`resource` 并启用 | 保持 `process`，warning | 保持 `lifecycle`，warning 缺失项 | **不失败**，warning 后继续 | warning；bash fail closed |
| `required` | 启用 | 若 `minimumLevel ≥ lifecycle` 则 fail closed | 若低过 `minimumLevel` 则 fail closed | fail closed（`minimumLevel=process` 由底线满足） | warning |
| `process` | 不启用，按 `process` 运行 | 按 `process` 运行 | 按 `process` 运行 | `minimumLevel > process` 时 fail closed | warning |
| `external` | 不启用 cgroup；本地可观察等级保持 `process`，warning 说明外部边界负责 | 同左 | 同左 | 不适用，不强制 | warning |

### 4.2 fail-closed 硬条件（`probeRunnerEnvironment` 产出 `error`，`detectRunnerEnvironment` 抛出）

```text
LEVELS[effectiveLevel] < LEVELS[minimumLevel]
  && mode !== 'auto'      // auto 只降级并 warning，永不失败
  && mode !== 'external'  // external 由外部边界负责
```

`required`/`process` 命中时，错误文案形如 `Runner isolation mode <mode> requires minimumLevel <minimumLevel>, but only <effectiveLevel> is available`。`auto` 命中时仅追加 warning：`effective isolation <level> is below minimumLevel <level>; auto mode continues at <level>`。

### 4.3 运行期 fail-closed

- 前台/后台 `bash` 一律经 `bwrap` 构造最小 rootfs；探测到 `bwrap` 缺失时命令 fail closed，绝不回落到 Host shell。
- Tool 可见性由 DSH composition/policy 决定；未适配的本地执行工具 fail closed，不静默落到 Host 文件系统。
- cgroup 开启时 `terminate()` 走 `SIGTERM` 由 cgroup 兜底回收；未开启时在非 Windows 平台对进程组 `process.kill(-pid, 'SIGKILL')` 处理 pre-setsid 竞态；Runner dispose 始终负责最终回收。
- Runner readiness 校验 `protocolVersion / runnerId / viewId / cwd === '/workspace'` 完全匹配后才允许绑定 Agent；不匹配即销毁。

## 5. `doctor`：生产诊断命令

`/censorfs-doctor` 是**只读、无副作用**的诊断命令（插件 `registerCommands` 注册，处理函数调用 `runtime.doctor()`）。

### 5.1 行为

- `runtime.doctor()` 调用 `probeRunnerEnvironment`（即最新一次完整探测），**永不抛错**：策略 fail-closed 时把原因放入报告 `error` 字段，同时保留其余全部细节供排障（与 `detectRunnerEnvironment` 抛出的同一 `error` 文案一致）。
- 报告额外携带 `legacyRunnerCgroup: boolean`，标注当前是否仍在走旧 `runnerCgroup` 兼容路径。
- 人读输出由 `formatDoctor` 生成，逐项列出：`mode · minimumLevel · effective`、fail-closed 结论、cgroup 委派根与 controllers、daemon socket（live / stale / 缺失）、`censorfs`/`mounter`/`node`/`bwrap` 路径、`/dev/fuse`，以及全部 warnings。
- 探测清单与启动探测完全一致（第 3.2 节），保证“doctor 说 FAIL 的，exploration 前检查也必然失败”。

### 5.2 安全承诺

- **禁止**：不创建 cgroup scope、不写 stateDir、不挂载、不 spawn Runner、不写 journal/Ticket、不启动 daemon。因此 doctor 可在生产实例上安全运行。
- 当前输出为文本摘要；机器可读 `--json` 输出与 exit code 契约规划中（见第 9 节）。

## 6. 实际隔离报告

“实际隔离报告”指三层输出，都必须反映**实际发生**的隔离，而不是配置请求：

### 6.1 探测报告（`probeRunnerEnvironment` 返回值）

```jsonc
{
  "requestedMode": "auto",
  "minimumLevel": "process",
  "effectiveLevel": "resource",   // 实际达到的层次；external 模式本地仍如实报告 "process"
  "cgroupEnabled": true,          // 实际是否启用 cgroup（探测结果，驱动 Runner）
  "controllers": ["memory", "pids", "cpu"],
  "cgroupRoot": "/sys/fs/cgroup/censorfs-runners", // 仅 cgroupEnabled 时出现
  "daemon": { "available": true, "socket": "...", "live": true },
  "commands": { "censorfs": "...", "mounter": "...", "node": "...", "bwrap": "..." },
  "fuse": true,
  "warnings": [],
  // "error": "..."  // 仅 fail-closed 时出现
}
```

`effectiveLevel` 与 `cgroupEnabled` 是“实际”判定：配置请求 `resource` 而 controller 缺失时如实给出 `lifecycle`；配置 `auto` 而探测到完整委派时如实给出 `resource`。

### 6.2 exploration 级报告（Session 事件快照）

`runtime.isolationSnapshot()` 把启动探测的快照写入 `exploration-started` Session 事件与 harness 日志（同一对象），包含 `requestedMode / minimumLevel / effectiveLevel / cgroupEnabled / controllers / cgroupRoot / warnings`（或 `error`）。事件重建后仍能审计“那次 exploration 实际以什么隔离运行”。

### 6.3 每 Runner 运行期报告（审计日志）

`RunnerManager.launch` 为每个 Runner 生成 `record`（`runnerId/viewId/runId/variantId`、`state` 迁移 `creating → ready → running → stopping → stopped`），并在启用 cgroup 时输出结构化 info：`scope=<runnerId> root=<root> stateDir=<stateDir> memoryMax/pidsMax/cpuMax=<限额或 ->`。dispose 后由 cgroup 侧完成 `cgroup.kill → populated=0 → rmdir`；这些日志与 6.2 的事件快照一起构成生产审计依据。

## 7. 旧配置兼容

### 7.1 `runnerCgroup` → `runnerIsolation` 映射

旧 `runnerCgroup` 继续被接受，作为归一化时的字段来源，且**语义不变**：

| 旧 `runnerCgroup` | 新 `runnerIsolation` 等价 |
|---|---|
| `enabled: true` | `mode: 'required'`、`minimumLevel: 'lifecycle'` |
| `enabled: false` / 未设置 | `mode: 'auto'`、`minimumLevel: 'process'` |
| `root` / `stateDir` / `memoryMax` / `pidsMax` / `cpuMax` / `cleanupTimeoutMs` | 同名字段直接沿用 |

归一化规则（`normalizeIsolationConfig`）：新 `runnerIsolation` 字段优先，旧字段兜底。`runnerCgroup.enabled` 只决定 mode/minimumLevel 的默认值；`index.js` 仍把 `runnerCgroup` 透传为 `config.runnerCgroup`（doctor 也通过 `legacyRunnerCgroup` 标记当前路径），避免破坏现有消费方。**推荐新部署直接写 `runnerIsolation`，`runnerCgroup` 仅作迁移期来源，后续版本可弃用**。

`stateDir` 校验规则同样生效于旧来源：配置 `root` 而未配置 `stateDir` 时抛 `TypeError`（`runnerIsolation.stateDir is required with root`）——与旧实现 `enabled: true` 时 `root/stateDir` 必填的语义一致。

### 7.2 `childCommand` 与 `inProcessOnly` 兼容

- `childCommand` 从必需配置中移除（`normalizeConfig` 的 `required` 数组不再包含它），因为 `branch_explore_inprocess` 不使用它：进程内 Runner 通过同一个 `mounterCommand` `exec` 当前 Node 与插件自带 `runner-process.js`。
- 新增 `inProcessOnly: true`：仅使用 in-process 路径的部署，`provider`/`model` 也不再必需（`required` 只剩 `socket`/`mounterCommand`/`controlPlaneCwd`）。
- 完整子 Harness 路径（`branch_explore`）仍在调用时校验：`inProcessOnly` 部署直接报错（`branch_explore is unavailable: ... use branch_explore_inprocess`）；否则 `childCommand` 未配置时报错（`branch_explore requires config.childCommand ...`）。mounter 降权后在 `/workspace` 以 argv 执行，`childEnv` 仍是唯一显式进入 worker 的 Harness 环境。
- 兼容保证：只配置 in-process 路径的新部署可省略 `childCommand`；既有部署保留它不影响 in-process 行为。

## 8. 生产与 TEST-ONLY E2E 安全边界

### 8.1 生产边界

- **cgroup v2**：只能使用专门委派给 CensorFS 的空子树（如 systemd unit `Delegate=yes` 后配置 `CENSORFS_CGROUP_ROOT`），**禁止**在任意 systemd-owned cgroup 中直接写 `cgroup.subtree_control`。supervisor 保持 root 且位于 workload scope 外；只有 worker 在 mount namespace 建立前写入 `cgroup.procs`。
- **stateDir**：固定 `0700`，创建与清理全程持 `flock` 状态锁；marker 记录 `(pid, process_start_time, boot_id, scope inode)`，用于 Host 死亡后安全识别与回收 stale scope；`recover_stale_locked` 在加锁下执行，避免并发重复回收。
- **回收**：正常退出、Host 死亡、stale scope 统一走 `cgroup.kill → 等待 populated=0 → rmdir`，`cleanupTimeoutMs` 上限兜底。
- **目标机版本**：若仍是 cgroup v1，必须保持 `CENSORFS_CGROUP_V2` 未设置（即 `auto`/`process`），或先迁移到统一 cgroup v2；探测期 reason 会明确提示 `no delegated cgroup v2 root configured`。
- **密钥与状态**：子 Harness 配置、Session persistence、密钥、模型日志必须位于 CensorFS workspace 外；`childEnv` 白名单转发，不转发整个 `process.env`。
- **监控**：MVP 不含在线 GC，未采用 Candidate 逻辑 Abort，对象空间与 cgroup 残留需监控（`/censorfs-doctor` 的只读报告可作为巡检入口）。

### 8.2 TEST-ONLY E2E 边界

`scripts/openeuler-namespace-runner-smoke.sh`、`openeuler-namespace-runner-cgroup-fault-smoke.sh` 与 `scripts/openeuler-dsh-inprocess-e2e.sh`（后者文件头部醒目标明 **TEST-ONLY**）只能在**独立测试环境**运行，与生产实例物理隔离：

- 运行前置条件即测试边界声明：要求 Linux、`jq/node/bwrap/setpriv`、`/dev/fuse` 字符设备；root 模式下强制非零 `CENSORFS_AGENT_UID/GID` 并用 `setpriv --clear-groups` 降权，非 root 必须 `sudo -n` 可用。
- 测试根 `mktemp -d "$test_parent/..."`（`CENSORFS_TEST_PARENT` 默认 `/var/tmp`），且强制 backing store 为本地 XFS/ext4——与生产约束一致，保证原子 rename/fsync 语义在测试中被真实覆盖。
- **E2E 绝不触碰**生产 socket、生产 cgroup 委派根、生产 stateDir。**自建资源无条件清理**：测试 daemon、临时 store/socket、测试根与 scratch 目录由 `trap ... EXIT` 统一回收——无论成败都杀死自建 daemon、删除自建临时目录，并 best-effort Abort 本轮打开/预备的 Ticket。existing daemon 模式（`CENSORFS_USE_EXISTING_DAEMON=1`）可**显式使用**已有 daemon/socket，但脚本**不接管**：不启动、不杀死、不清理其任何资源，只对自建 Ticket 做 best-effort Abort。
- fault smoke 额外演练：scope 创建失败回滚（移除已建 scope 与 marker）、supervisor 死亡后 stale 回收、`populated=0` 等待与超时清理、限额写入失败时的清理路径。
- `openeuler-dsh-inprocess-e2e.sh`（TEST-ONLY）验证 **Runner 协议与 CensorFS 控制面**（**不调用 DSH/LLM**）：自建**临时** store/socket/daemon（`trap ... EXIT` 清理），或经 `CENSORFS_USE_EXISTING_DAEMON=1`/已存在 `CENSORFS_SOCKET` 复用现有 daemon（不启动、不杀死）。脚本自身**复刻**生产前置探测（daemon socket 活性 + `head` 往返、`censorfs`/`censorfs-mounter` 可执行、`/dev/fuse`、内核与权限前置），而不是调用插件内部的 `detectRunnerEnvironment`（Host 插件代码，shell 脚本无法导入）；随后驱动真实 in-process Runner JSON-RPC（health/fs.\*/process.run/job.\*/shutdown）与 `variant-open/prepare/abort`、只读 Candidate View 验证。**脚本不创建或回收任何 cgroup scope**——cgroup 生命周期/限额演练由 `openeuler-namespace-runner-cgroup-fault-smoke.sh` 在独立临时根覆盖。模型驱动的 `branch_explore_inprocess`（真实 Agent + LLM）不在本脚本内，需显式另跑（脚本结尾会提示启动 profile 的方式）。绝不写 sudoers、绝不 mount cgroup、绝不改系统配置。
- 结果语义：E2E 证明“生产边界设计在目标机成立”；**不**把 E2E 用作生产实例上的例行检查（那是 `/censorfs-doctor` 的职责）。Windows 开发环境只验证 Rust 控制面、事件折叠与 JSON 契约；FUSE/mount namespace/AArch64 与完整模型调用必须在目标机验收。
- `openeuler-dsh-inprocess-real-e2e.sh` 是发布门禁：要求操作者显式提供真实 DSH 命令和 Session JSON 快照，检查 `branch_explore_inprocess` 的 2–4 个 Variant 完成 `Compare → Publish`；它不会替代三项 TEST-ONLY smoke，也不会猜测 Harness CLI 或凭据。

### 8.3 边界对照表

| 维度 | 生产 | TEST-ONLY E2E |
|---|---|---|
| cgroup 根 | 已委派生产子树（`Delegate=yes`） | e2e 脚本不创建/回收任何 cgroup scope；cgroup 生命周期演练仅由 `openeuler-namespace-runner-cgroup-fault-smoke.sh` 在临时根执行 |
| stateDir | 生产 `/run/censorfs/cgroup-state` | 同上（cgroup-fault-smoke 在测试根下使用临时 stateDir） |
| socket | 生产 control socket | 测试根内 daemon 自建 socket |
| 身份 | supervisor root、worker 降权 | root 或 sudo + setpriv 降权，root 模式强制非零 uid/gid |
| 数据 | 真实 workspace、真机验收 | `mktemp` 测试树；自建数据无条件清理（失败亦清理） |
| 例行检查 | `/censorfs-doctor`（只读） | smoke 脚本（一次性验收） |

## 9. 现状与演进

- **已落地**：`runnerIsolation` 归一化与旧 `runnerCgroup` 兼容；启动期自动探测并驱动 Runner cgroup 开关；exploration 前 fail-closed 检查；`isolationSnapshot` 写入 Session 事件；auto 运行期 launch 失败降级到 process 并重试一次（`isolation-fallback` 事件 + 日志，fold/客户端同步）；`/censorfs-doctor` 只读命令与文本报告；`inProcessOnly` 与 `childCommand` 调用时校验。
- **规划中**：doctor 的机器可读 `--json` 输出与退出码契约；每 Runner 实际隔离审计记录的落盘（当前为结构化日志）；`external` 模式外部边界证据的机器可读注入（如 env/配置文件声明）；cgroup v1 迁移路线与 `cpu.max` 双值格式在部分内核上的校验；嵌套 Runner（`inProcessMaxDepth > 1`）的 cgroup scope 归属与回收顺序（提高默认值前须完成专项测试）。
