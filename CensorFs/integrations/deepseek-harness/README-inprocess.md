# CensorFS × DeepSeek Harness — In-Process Subagent Model

> **⚠️ 历史文档**：本文描述的"宿主内子代理 + 工具参数路径重写"架构已被
> `feature/in-process-runner-local` 合并带来的 **Namespace Runner 模型**取代
> （见 README.md「两条路线的边界」）。所述的路径重写层与 P0 沙箱补丁
> （`in-process-path-rewrite.js`、`in-process-runtime.js`）已从树中删除，git 历史可查。
> 保留本文仅作设计决策与实测记录存档。

> 分支 `feature/in-process-subagents`。在 Parallel Worlds MVP（进程外子 Harness）之外，
> 本分支新增**进程内子代理**模型：子代理是宿主进程里的协程，通过 `ctx.subagents` 的
> 自定义 provider 创建。

## 设计一句话

**逻辑 cwd = 共享路由器挂载点（`/workspace`）；私有视图挂载点 = 宿主进程 fs 工具的数据路径。**

- 插件为每个子代理开 ticket/view → `censorfs-mounter --no-namespace --target <私有挂载点>` 原地挂载（数据路径）；
- 惰性确保**共享路由器挂载** `/workspace`（trunk = 当前分支 generation 只读视图）；
- provider 以 `meta.cwd = /workspace`（逻辑 cwd）创建进程内子代理 → bash 沙箱根锚定在 `/workspace`，
  沙箱不会在 FUSE 之前拒绝 `/workspace` 写入；
- 子代理 spawn 的 shell 进程 cwd 在 `/workspace` 内，环境注入 `DSH_SESSION_ID = session.header.id`，
  daemon 按 pid 路由到该子代理的视图；
- 宿主进程内执行的 fs 工具（read/write/edit/read_image/glob/grep）由**工具参数层重写**把相对路径与
  `/workspace` 绝对路径翻译到私有挂载点 = 子代理只看到自己的视图。

## 新增组件

| 组件 | 职责 |
|---|---|
| `src/in-process-provider.js` | 进程内 provider：`ctx.agents.create({ meta: { ...childSessionMeta(...), cwd: 逻辑 cwd(/workspace) } })`，单轮驱动 + 结果读取；维护 `sessionKey(header.id)→binding` 映射供路径重写归属判定；创建后/首次 followup 前 `registerSession`，dispose 后 `unregisterSession` |
| `src/in-process-runtime.js` | 生命周期：open ticket/view → 私有原地挂载 → 运行子代理 → Prepare/Abort → 卸载私有挂载；惰性共享路由器挂载（`ensureRouter`，trunk = 当前 generation 只读视图）；`registerPid`/`unregisterPid`/`pidRoutes` 与 `registerSession`/`unregisterSession`/`sessionRoutes` 控制面助手 |
| `src/in-process-path-rewrite.js` | **工具参数层重写**：订阅 `tools/execute`，把绑定子代理的 fs 路径字段（相对 + `/workspace` 绝对）翻译到私有挂载点；bash `workdir` 保持/归一化到 `/workspace`（路由器目标，见下节）；**对 `write`/`edit` 追加按调用的沙箱可写根覆盖**（ALS 帧 + `sandboxPolicy.resolve` 补丁，把本次调用的 `workspaceRoot` 换成私有挂载点），修复 fs 写操作被宿主文件沙箱拒绝的 P0 问题 |
| 工具 `branch_explore_inprocess` | 一次调用 = 一个进程内子代理在它自己的视图里探索，成功后产出不可变 Candidate（可合并） |

### 配置新增（cordis.patch.yml 可选）

```yaml
      config:
        # ...原有 Parallel Worlds 配置...
        inProcessMountRoot: /var/tmp/censorfs-inprocess   # 私有视图挂载点根目录（fs 工具数据路径）
        inProcessPathRoot: /workspace                     # 约定根（工具参数层重写的翻译源）
        inProcessRouterTarget: /workspace                 # 共享路由器挂载点（宿主 workspace，逻辑 cwd）
        ctlCommand: censorfsctl                             # pid/session 路由控制命令；默认 $CENSORFS_CTL，否则从 $CENSORFS_MOUNTER 推导
```

## 共享路由器挂载 + session 路由（进程内身份路由的完整闭环）

`InProcessRuntime` 在第一次 `branch_explore_inprocess` 时**惰性**确保一个**共享路由器挂载**
（`censorfs-mounter --no-namespace --router --target <inProcessRouterTarget>`，默认 `/workspace`），
trunk 视图 = **当前分支 generation 的只读视图**（`cli.openGeneration(head.generation_id)`）。并发调用通过
`routerPromise` 去重共享同一挂载；`dispose()` 卸载路由器并关闭 trunk 视图（先 umount 再 close，避免路由会话
收到已关闭视图的请求）。

**路由器挂载在 VFS 层必须可写 —— 绝不传 `--read-only`**（`smoke-inprocess.sh` /
`openeuler-session-router-smoke.sh` 同样如此，并在 mountinfo 中断言 `/workspace` 为 `rw`）：
被路由到 ticket 视图的进程要经 `/workspace` 写入；若整个挂载被内核置为 `MS_RDONLY`，一切路由写入都会被挡下。
trunk/未知 pid 的只读来自 **trunk generation 视图 `can_write=false` → FuseViewAdapter 返回 EROFS**，
而非挂载本身。挂载成功后插件会读取 `/proc/self/mountinfo` **校验目标不是 `ro`**，若为 `ro` 则报错并回滚
（卸载 + 关闭 trunk 视图）。

路由器上的每个 FUSE 请求按调用者 pid 解析：**显式 pid 路由 → 进程自身的 `DSH_SESSION_ID` → 祖先进程的
`DSH_SESSION_ID`（有界父链回退，≤16 层；覆盖 `env -i` 清空环境的后代）→ 进程/祖先 cwd 对注册的
mount_point 最长前缀匹配 → trunk 只读视图**。

- **session 键 = `session.header.id`**（`sessionKey(session) = String(session?.header?.id ?? session?.id)`）：
  DSH shell 环境注入的正是 `execution.agent.session.header.id`，daemon 从 `/proc/<pid>/environ` 读到的
  `DSH_SESSION_ID` 与 `registerSession` 的键必须一致；`boundSessions` 与 `bindingForAgent` 用同一 helper。
- 插件在**子代理创建后、首次 followup 前** `registerSession(sessionId, viewId, mountPoint)`（尽力而为，
  失败仅告警）；在**子代理 dispose 后** `unregisterSession(sessionId)`。
- `session-routes` 子命令 / `sessionRoutes()` 用于排障。

### 挂载安全

- 挂载前（路由器目标与私有挂载点都一样）先检查目标是否**已是挂载点**
  （`mountpoint -q <target>`；二进制缺失时回退解析 `/proc/self/mountinfo` 第 5 字段）。
- 已挂载 → **拒绝**覆盖挂载并给出明确报错：本运行时绝不静默 overmount，也绝不 umount 一个它没有挂载过的挂载点。
- **路由器挂载绝不传 `--read-only`**（VFS 层必须可写），挂载后校验 `/proc/self/mountinfo` 目标不是 `ro`；
  只读语义只来自 trunk generation 视图 `can_write=false` → EROFS。
- `dispose()` 只卸载**本运行时创建**的挂载（`this.router` 记录的路由器挂载 + `mounts` 里的私有挂载点）。
- **已知限制（陈旧挂载恢复）**：daemon/插件崩溃留下的陈旧挂载与外来挂载无法区分，插件不会自动接管；
  需人工恢复（如 `umount /workspace`，或按 `/proc/self/mountinfo` 清理），之后重试即可。

## 工具参数层重写（进程内身份路由）—— 绝对/相对路径盲区的答案

进程内子代理是宿主协程：宿主进程内执行的 fs 工具共享宿主 pid，`pid 路由`无法按 pid 区分；
会话 cwd 是 `/workspace`（路由器），宿主 pid 的 `/workspace` 读写会落到只读 trunk。
**工具参数层重写**在工具执行入口解决它：

- DSH 工具注册表在工具体执行前发 `tools/execute` **waterfall** 事件，payload 是
  `MutableToolRunContext`（可整体替换 `arguments`），并携带 `exec.agent`（调用者身份）；
  事件按 agent 作用域路由，**插件在无作用域 ctx 上订阅能收到所有 agent 的调用**。
- 插件监听器：`provider.bindingForAgent(exec.agent)` 查 `sessionKey→binding`（子代理创建时登记）
  → 命中则重写参数 → `next()`。
- **显式按工具字段表**（`TOOL_PATH_POLICY`），不做全量字段名扫描：
  - **fs 工具**（read/write/edit/read_image 的 `file_path`）、**glob/grep 的 `path`**、
    **glob 的 `pattern`**：`/workspace`（`<root>`）绝对值与**相对值**都翻译到私有挂载点；
    相对值经 `path.resolve` + 前缀检查，`..` 会逃出私有挂载点的值不重写（保持原样）；
    `/workspace` 之外的绝对路径不触碰；**grep 的 `pattern`（正则）绝不触碰**。
  - **bash `workdir`**：保持**逻辑 `/workspace` 语义**（缺省 = 会话 cwd = `/workspace`；
    相对值在 `/workspace` 下归一化；`/workspace` 前缀原样；`..` 逃逸不重写；绝不翻译到私有挂载点）——
    派生 shell 从共享路由器内启动，沙箱根（= 会话 cwd = `/workspace`）放行，随后 daemon 按
    `DSH_SESSION_ID`（或祖先进程的）把该 shell 的 pid 路由到子代理视图；
    私有挂载点仅是宿主进程 fs 工具的数据路径。
  - 未列入的工具、以及列入工具但未列出的字段，一律原样通过。
- 非绑定会话（父代理）、bash command 字符串内嵌路径一律不触碰（后者由沙箱 cwd 锚定 +
  `DSH_SESSION_ID` 路由兜底）。
- **模型无感**：`tool/call` 记录的是模型写的原始值，工具实际执行在重写后的路径上。

### fs 写沙箱覆盖（P0 修复）

重写后的真实路径在私有挂载点下，而 DSH 文件沙箱的 `workspace-write` 边界是**会话 cwd =
逻辑 `/workspace`（路由器目标）**——真实路径落在可写根之外，`write`/`edit` 一律被拒
（`FS_SANDBOX_DENIED`；实测中子代理只能退化为 bash heredoc 写入）。读操作不受影响。

修复（纯插件实现，`src/in-process-path-rewrite.js`）：

- 文件沙箱的策略是**按调用**解析的：`sandboxPolicy.resolve({ session })` 的产物随
  `writeText`/`editText` 一路传到 `checkedTarget`，凡 `policy.workspaceRoot` 前缀下的目标即放行。
- 插件对 `write`/`edit` 调用在 `next()` 外套一帧 AsyncLocalStorage（携带私有挂载点），并一次性
  猴补共享的 `sandboxPolicy` 服务实例的 `resolve`：帧内把解析结果的 `workspaceRoot` 换成挂载点，
  `mode`/`sessionId` 原样保留（升级审批流不受影响）。
- **作用域严格受限**：帧只包住绑定子代理的 `write`/`edit` 这一次调用——bash 与其它工具照旧按
  逻辑 `/workspace` 解析（bash 沙箱根不变，pid 路由路径不受影响）；无帧的解析逐字段不变。
- **只收窄不放宽**：可写根从 `/workspace` 换成更小的私有挂载点，未重写的目标（约定根之外的绝对
  路径）依旧被拒。
- `sandboxPolicy` 服务实例被 tool-fs/tool-bash 在构造期捕获，因此后 provide 无效，必须原地补丁；
  补丁有 symbol 幂等护栏；旧版组合缺服务时静默降级为修复前行为。

这是纯插件实现（`ctx.on('tools/execute')`），**零 deepseek-harness 改动**；
机制已被 DSH 内置策略（checkpoint/timeout policy）用同一事件验证过。

## pid 路由（机制 B）—— 已在 CensorFS 核心落地并真机验证

- **CensorFS**（本分支 Rust 改动）：
  - `censorfs-mounter` 新增 `--no-namespace`/`--target`（原地挂载）、`--router`（路由会话）、可选 command；
  - `NamespaceConfig::validate` 放宽为任意绝对挂载目标；
  - daemon 新增 `PidViewTable`（pid→view）+ 控制面 `register-pid`/`unregister-pid`/`pid-routes` + 特权握手 `ATTACH_ROUTER`；
  - 数据面 `RouterViewAdapter`：按 FUSE 请求携带的调用者 pid 查表路由（未注册 → trunk 视图），
    所有视图适配器强制零 TTL（避免单挂载下内核 dcache 串视图）。
- **真机验证**（openEuler 24.03 AArch64，`scripts/smoke-inprocess.sh` 思路）：
  未注册进程读 `/workspace` → trunk 内容；注册 pid 的进程写/读 `/workspace` → 落到它自己的 ticket 视图；
  未注册进程仍见 trunk（隔离成立）；原地挂载 + command 写视图不影响 trunk。

### DSH 侧自动归属（待接入，属 deepseek-harness 仓库）

进程内子代理 spawn 的进程要**自动**注册到正确视图，需要在 DSH 的 `dsh-subprocess-local`
增加 spawn 归属 hook（发 `subprocess/spawned { pid, agentId }` 事件），插件监听后
合成 `pid → view` 并经 `register-pid` 推给 daemon。改动点：

`packages/subprocess/subprocess-local/src/index.ts`：

```ts
// LocalSubprocessRuntime 增加字段与方法：
attribution = new Map<number, { agentId: string; startedAt: number; argv0: string; kind: 'spawn' | 'terminal' }>()

/** spawn()/spawnTerminal() 里，进程创建时： */
const owner = this.ctx.get('agents')?.currentInitiator?.()  // AsyncLocalStorage：当前发起子代理
if (owner !== undefined) {
  handle.ownerAgentId = owner.id
  this.attribution.set(handle.pid, { agentId: owner.id, startedAt: Date.now(), argv0: spec.argv[0], kind: 'spawn' })
  this.ctx.emit('subprocess/spawned', { pid: handle.pid, agentId: owner.id, argv0: spec.argv[0], kind: 'spawn' })
}
// release 时：
this.attribution.delete(handle.pid)
this.ctx.emit('subprocess/exited', { pid: handle.pid })

// 查询 API（可选）：
ownerOf(pid: number) { const e = this.attribution.get(pid); return e === undefined ? undefined : { ...e } }
listAttributed() { return [...this.attribution.entries()].map(([pid, e]) => ({ pid, ...e })) }
```

插件侧监听：

```js
ctx.on('subprocess/spawned', async ({ pid, agentId }) => {
  const viewId = bindings.get(agentId)          // agentId → view（插件注册表）
  if (viewId !== undefined) await runtime.registerPid(pid, viewId)
})
ctx.on('subprocess/exited', async ({ pid }) => { await runtime.unregisterPid(pid) })
```

## 安装 peer 依赖（`dsh plugin add` 的 link: 坑，必读）

`dsh plugin --profile <p> add <插件目录>` 底层走 pnpm，对目录安装默认用 **`link:`（符号链接）**。
Node ESM 从链接的**真实路径**（插件目录本身）解析 bare import，而插件目录旁没有任何
`node_modules`，启动即失败：

```text
Error [ERR_MODULE_NOT_FOUND]: Cannot find package '@deepseek-ai/dsh-llm'
  imported from .../integrations/deepseek-harness/src/index.js
```

修复（实测 openEuler 24.03 / dsh 0.1.0-rc.8 / pnpm 11.22.0）：

```bash
P=~/.dsh/profiles/headless   # 换成你的 profile
cd "$P"
# 1) 改用 file:（拷贝语义），插件落进 profile 的 node_modules，peer 可从 profile 树解析
pnpm remove @censorfs/deepseek-harness
pnpm add "file:/绝对路径/integrations/deepseek-harness"
# 2) 补齐缺失的 peer（缺哪个装哪个；国内可加 --registry=https://registry.npmmirror.com）
pnpm add @deepseek-ai/dsh-sdk-client@0.1.0-rc.8
pnpm add @deepseek-ai/dsh-sdk-protocol@0.1.0-rc.8
```

装完用入口导入探针确认（期望 `ENTRY_IMPORT_OK`）：

```bash
node --input-type=module -e "
import { realpathSync } from 'fs';
import { createRequire } from 'module';
const require = createRequire(import.meta.url);
const real = realpathSync(require.resolve('@censorfs/deepseek-harness/package.json'));
const dir = real.replace('/package.json','');
import('file://' + dir + '/src/index.js')
  .then(() => console.log('ENTRY_IMPORT_OK'))
  .catch((e) => console.log('FAIL', String(e.message).slice(0, 200)));
"
```

版本说明：插件 peerDependencies 固定 `0.1.0-rc.7`，rc.8 运行时实测可正常加载与运行
（缺失的 peer 直接装运行时同版本即可）。注意 `file:` 安装是**拷贝**，改插件源码后需重跑
`pnpm add file:...` 刷新。

## 验证

```bash
# Rust（真机）
cargo check --workspace && cargo test -p censorfs-core          # 含 pid/session 路由控制面单测
cargo build --release
bash scripts/openeuler-inprocess-smoke.sh                      # FUSE 真机冒烟（路由 + 原地挂载）
bash scripts/openeuler-session-router-smoke.sh                 # 零模型 Session Router 冒烟（env/cwd/父链归属）

# JS（本机，需要 node）
npm install && npm test                                        # 现有 + in-process-runtime 单测
```

### 实测状态（openEuler 24.03 AArch64，v2）

- **Rust（v2 核心）**：`cargo check --workspace` ✅；单测 session 5/5、pid 3/3、fuse adapter 4/4
  （含 stat 解析与**有界父进程链边界**）✅；`cargo build --release` ✅（主机 rustfmt 不可用）。
- **零模型 Session Router 冒烟**（`scripts/openeuler-session-router-smoke.sh`）✅ SMOKE PASSED：
  env `DSH_SESSION_ID` 归属（bash + python 绝对 `/workspace` 读写）、cwd 最长前缀兜底、
  **父进程链兜底**（env-cleared 孙进程）、未知 pid 只读 trunk（写 EROFS）、unregister 回落、
  控制面负例（未知视图 / 相对 mount-point / 空 session id）。
- **JS 插件单测**：41/41（censorfs-cli 1 + events 2 + path-rewrite 25 + in-process-routing 13）。
- **付费 spawn 绝对路径 e2e**（`scripts/e2e-spawn-abs-path.sh`）✅：runId
  `inprocess-f454d090-4aaf-45bb-8a1b-7b3d73789a89` / viewId `c6648886-c896-434c-8eac-c1bc00cd0450` /
  ticketId `4de89c50-9201-4d30-bee5-29262b9077aa` / candidateId `c7c4bb39-bf13-4fd1-bd66-ea1e688c8441` /
  child session / SID `f437bf4b-9413-47a2-b11e-8ca411a4b905`。子代理仅一次 bash 调用，命令为
  `python3 -c 'import os;open("/workspace/abs-e2e.txt","w").write("router-e2e-marker");print("SID="+os.environ["DSH_SESSION_ID"])'`
  （bash 调用 = 1、read/write/edit 调用 = 0、tool errors = 0、EROFS = 0、归档 header cwd =
  `/workspace` 且 header id = 打印 SID）；dsh 退出后 `/workspace` 路由挂载与 session 路由均已回收
  （Router unmounted + `session-routes` 不再含子代理 session id，provider dispose 注销生效）；
  candidate 只读挂载中 `abs-e2e.txt` 内容**精确等于** `router-e2e-marker`。
### 实测状态（fs 写沙箱覆盖修复后，openEuler 24.03 AArch64 复测）

- **安装修复**：`dsh plugin add` 的 link: 坑按 §「安装 peer 依赖」流程实测通过
  （`file:` 重装 + 补装 `dsh-sdk-client`/`dsh-sdk-protocol`），入口导入探针 `ENTRY_IMPORT_OK`。
- **强制 fs 工具改码 e2e**：子代理 6 次工具调用（read×2 + **edit×2** + bash×2，bash 仅以
  `/workspace` 为 workdir 跑 unittest）——**isError = 0、真实沙箱拒绝 = 0、EROFS = 0**；
  修复前同一场景下 edit/write 均被宿主沙箱拒绝（子代理只能退化为 bash heredoc 写入）。
- edit 直改 `/workspace/payment.py` 成功落视图 → candidate 只读验证 unittest OK →
  `censorfsctl publish`（CAS）head_seq 0→1 → main 内容含修复且 unittest OK →
  退出后 session-routes 清空、censorfs 挂载数归 0。

- **诚实限制（保留）**：后台**进程树清理未闭环**（`run.dispose` 的 spawn pid 注销未验证）；路由
  挂载存活期间宿主 `/workspace` 被遮蔽（未归属写回落只读 trunk → EROFS，部署约束见详设
  §7.1.6e-13）；**trunk 快照陈旧**（路由会话挂载时固定，publish 后不刷新，直至会话重建）；
  路由是**协作归属而非安全边界**（env 可伪造 `DSH_SESSION_ID` 伪装他人 session）。
