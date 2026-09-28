# CensorFS × DeepSeek Harness — Parallel Worlds（FUSE 隔离耦合版）

这套目录是 CensorFS 自带的 DeepSeek Harness integration bundle，提供两条并存路径：默认 `branch_explore` 由 `censorfs-mounter` 把每个 Variant 的完整 Harness 子进程放进独立 CensorFS mount namespace；`branch_explore_inprocess` 则把 Agent/模型上下文留在 Web Host，只把文件与 shell 执行委派给专属 Namespace Runner。两条路径都使用真实 `/workspace`，而 Harness 会话、模型凭据、日志和缓存仍在 workspace 外。

**共同 FUSE 数据面**：每个 variant 一个独立 CensorFS View（copy-on-write 覆盖层），写入私有 Upper/Delta；`prepare`→Candidate→`publish` 才把 delta 合并成新 Generation。无共享文件系统模式、无 git worktree 模式。

当前适配 DeepSeek Harness `0.1.0-rc.8`（npm 包 `@deepseek-ai/*@0.1.0-rc.8`）。Harness 仍是 developer preview；升级前必须重跑这里的 unit test、Session event snapshot 和 openEuler 真机 smoke。

## 架构：三个协同件

这个 integration 由三个协同件组成，运行时通过 Unix socket 协议耦合（不是打包成一个 bundle）：

```
Web profile（父进程，控制面）
  └─ censorfs-parallel-worlds 插件（本目录 src/ + client/）
       ├─ FuseProvider (src/fuse-provider.js)
       │     ├─ 建 Unix socket server，argv 透传 --event-socket <path>
       │     ├─ spawn 子进程 → censorfs-mounter → bin/dsh-jsonrpc-agent
       │     └─ 往 socket 写控制 JSON 行（cancel / update-policy）
       │
       └─ Headless profile（子进程，每个 variant 一个）
             ├─ bin/dsh-jsonrpc-agent —— 一次性 sh 适配器壳（见下）
             │     ├─ 重建被 sudo env_reset 抹掉的环境
             │     ├─ 解析 --model/--provider，生成 --patch 覆盖 agent-default-model
             │     └─ export DSH_EVENT_SOCKET，exec dsh --profile headless
             │
             └─ event-exporter 插件（本目录 event-exporter/）
                   ├─ ctx.on('session/event') → socket 写 JSON 行（事件流出）
                   └─ sock.on('data') → agent.cancel() / agent.steer()（控制流入）
```

**关键**：双向控制（cancel / update-policy）的逻辑在 `event-exporter` 里（调 DSH 的 `agent.cancel`/`agent.steer`），**不在 `dsh-jsonrpc-agent`**。

### `bin/dsh-jsonrpc-agent` 是什么

它**不是**原设计假设的那个"完整 JSON-RPC session 协议二进制"——DeepSeek Harness 从未发布过该二进制。它是一个**phase-1 一次性 POSIX sh 适配器壳**，干三件实事：

1. **sudo env_reset 补救**：mounter 经 `sudo -n` 跑，`env_reset`+`secure_path`+`always_set_home` 把 FuseProvider 构造的环境全抹了（PATH 没 node、HOME 变 /root）。壳重建 `HOME/DSH_HOME/PATH`，否则子进程连 `dsh` 都找不到。
2. **模型透传**：argv 能穿过 sudo（env 不行），壳解析 `--model/--provider`，生成 `--patch` YAML 覆盖 headless 的 `agent-default-model`。
3. **事件 socket 桥接**：把 `--event-socket` 转成 `DSH_EVENT_SOCKET` 给 event-exporter 用。

范围：task 经 `--task-file` 传入（argv 指向的共享文件）、result 经 `--result-file` 回传；stdio 仅作回退。原因是宿主 Harness 会对 plugin spawn 的子进程 stdio 做协议化接管（socketpair 中继，不转发裸 stdin/stdout 字节），文件通道在两种环境下都可靠。事件与控制仍走 event socket。它自身没有 session.run/event/cancel 协议——phase 2 双向控制走 event-exporter socket 的反向通道。

## 已落地的闭环

- `/explore [2-4] [--mode fuse|inprocess] <task>`：让 Coordinator 生成真正不同的策略并调用探索工具；缺省 `--mode` 时走 `branch_explore`（FUSE 轨）。
- `branch_explore`：从同一 `(Generation, head_seq)` 创建 2–4 个 Variant，完整子 Harness 并行修改真实文件。
- worker 成功后 `Prepare` 成不可变 Candidate；失败、取消和超时自动 `Abort`。
- Candidate 验证在新建的只读 View 中执行；每项验证使用独立 View，构建和临时目录指向 `tmpRoot/<run>/<variant>`。验证结果三态：`passed` / `failed` / `unvalidated`（未配置验证 profile 时跳过验证）。
- Session 持久事件重建竞技场；刷新页面不依赖内存中的 worker 对象。
- Web 卡片：Git Graph 分支图 + Tab 化 Variant 详情（活动 / 系统级 / 验证 / 文件变更 / Agent 总结）+ 对比表 + 全局子代理图（纵向树形泳道 + 时间流）。
- "预览""采用""放弃"按钮调用现有 `commands` Host Remote；不产生模型调用。采用前服务端从当前 Session 事件重建 Run，再校验 Candidate 与最初 Head。
- 发布遇到 Head 变化时记录 `variant-stale`，Candidate 保留，不覆盖新 Head。
- 静态站点预览使用临时只读 Candidate mount，只绑定 `127.0.0.1`，默认十分钟自动关闭。
- **双向控制**：`/censorfs-abort` 经 socket 发 `cancel`，子进程 agent 优雅取消（3s 内不退则 SIGTERM 兜底）；`/censorfs-policy` 经 socket 发 `update-policy`，子进程 `agent.steer` 注入策略指令，运行中改向，不重启。

排名规则是一个可解释的 MVP Judge：`passed` 与 `unvalidated` 两个层级从高到低排序，仅淘汰显式 `failed` 或 `requiredPassed` 不通过的方案；同层内再按失败检查数、变更路径数和耗时排序。主 Agent 仍应结合任务语义解释优劣；排名不会自动发布。

## 安装（当前唯一路径：从源码仓）

插件尚未发布到 npm registry——**唯一受支持的安装方式是从源码仓打包后 tarball 安装**。目标机须为 openEuler/Linux，具备 `/dev/fuse`、mount namespace 与 XFS/ext4 backing store。

```bash
# 0) 获取源码（安装机与构建机可以是同一台）
git clone https://gitcode.com/cloudyyy1234/BranchorFS
cd CensorFS

# 1) CensorFS 二进制（censorfs / censorfsd / censorfs-mounter）
#    有 release 归档： CENSORFS_RELEASE_URL=… ./scripts/install-censorfs-binaries.sh
#    没有归档就本地构建：source ~/.cargo/env && cargo build --release --locked
#                        然后自行 install 到 $PATH 或改 CENSORFS_COMMAND/MOUNTER 指向 target/release

# 2) 打包两个插件 tarball（主插件 + event-exporter）
./scripts/package-explore.sh ./dist/explore

# 3) 目标机部署（安装适配器、装进 web/headless profile、写出环境文件）
#    凭据文件必须自备：/secure/.credentials.yaml（dsh 官方凭据层，子进程经 sudo env_reset 后唯一可靠的 key 来源）
DSH_CREDENTIALS_FILE=/secure/.credentials.yaml ./scripts/deploy-explore.sh ./dist/explore
source ./censorfs-explore.env

# 4) 数据面
./integrations/deepseek-harness/demos/run-demo.sh code-fix /some/xfs-or-ext4/dir   # init store + daemon + demo
```

> Registry 直装（`dsh plugin --profile web add @censorfs/deepseek-harness` 等）在插件发布到 npm/gitcode packages **之后**才可用；发布前请勿使用该写法。

## 打包与部署

仓库根目录提供可重复的双插件打包和目标机部署入口。打包会生成主插件与 `event-exporter` 两个 tarball，并为每个包生成 SHA-256 文件；tarball 安装不会触发本地目录安装的 `link:` peer 依赖陷阱：

```bash
# 在 Linux/openEuler 构建机执行
scripts/package-explore.sh ./dist/explore

# 在目标机执行；第二个参数可省略，默认使用仓库内 dist/explore
DSH_CREDENTIALS_FILE=/secure/path/.credentials.yaml \\
  scripts/deploy-explore.sh ./dist/explore
source ./censorfs-explore.env
```

`deploy-explore.sh` 会安装 `dsh-jsonrpc-agent`、将主插件装入 `web` profile、将 `event-exporter` 装入 `headless` profile，并写出最小环境文件。它**不会**把 `DEEPSEEK_API_KEY` 写入文件；由于子进程可能经过 sudo `env_reset`，必须通过 `DSH_CREDENTIALS_FILE` 提供 dsh 官方 `.credentials.yaml`，缺失时部署直接失败。CensorFS 三个 Rust 二进制仍由 `scripts/install-censorfs-binaries.sh` 单独安装。

打包与部署完成后，先执行无模型的命令/文件检查，再按“验证”章节运行真实 `branch_explore`；真实 explore 会产生模型费用。

### 从旧安装升级（目录/file: 时代 → tarball）

早期版本用 `file:` 目录方式安装，profile 里可能留着一个**陈旧实体副本**（改源码不生效、甚至会缺新版文件）。升级步骤：

```bash
export DSH_HOME=$HOME/.dsh
dsh plugin --profile <web|headless> remove @censorfs/deepseek-harness
rm -rf "$DSH_HOME/profiles/<profile>/node_modules/@censorfs/deepseek-harness"   # 关键：清掉陈旧副本
dsh plugin --profile <profile> add /path/to/censorfs-deepseek-harness-0.1.1.tgz
# 验证加载的是新代码（应含 fuse-provider.js 与文件通道标记）：
grep -c 'task-file\|taskFilePath' "$DSH_HOME/profiles/<profile>/node_modules/@censorfs/deepseek-harness/src/fuse-provider.js"   # 期望 ≥ 1
```

同版本号 tarball 会被 pnpm store 复用导致"看起来装了但没换"，所以务必先 `remove` + 删副本再加。

### 发布自测清单

改动提交前，本地与目标机按下表自测（全部免费，除最后一条）：

| # | 检查 | 命令 | 期望 |
|---|---|---|---|
| 1 | 插件单测 | `cd integrations/deepseek-harness && node --test --test-isolation=none` | 全绿（Windows）；Linux 可去掉 `--test-isolation` |
| 2 | 适配器语法 | `sh -n integrations/deepseek-harness/bin/dsh-jsonrpc-agent` | 无输出 |
| 3 | Rust 编译 | `cargo build --release --locked` | 成功 |
| 4 | Rust 测试 | `TMPDIR=/var/tmp cargo test --locked`（backing 必须 XFS/ext4，/tmp tmpfs 会被拒） | 全绿（core_flow 20/20） |
| 5 | 打包 | `scripts/package-explore.sh` | 两个 tarball + `.sha256` |
| 6 | 部署冒烟 | `DSH_CREDENTIALS_FILE=… scripts/deploy-explore.sh` | web/headless 入口探针 `ENTRY_IMPORT_OK` |
| 7 | 适配器冒烟 | `echo 'reply: OK' \| $DSH_CENSORFS_CHILD_COMMAND --model … --provider …` | 输出 `OK`、exit 0 |
| 8 | 真机 explore（可选，花钱） | 见“验证”章节 | 排名返回、候选可查、发布走 CAS |

### 从本地目录安装时的 peer 依赖坑

`dsh plugin`/pnpm 以目录方式安装本地插件时默认生成 `link:` 符号链接，运行期解析不到 peer 包会报 `Cannot find package '@deepseek-ai/dsh-llm'`。优先使用上面的 `pnpm pack` + tarball 流程；若必须从目录安装，则改用 `file:` 复制安装并补齐 peer：

```bash
pnpm add file:/path/to/CensorFS/integrations/deepseek-harness
pnpm add @deepseek-ai/dsh-sdk-client@0.1.0-rc.8 @deepseek-ai/dsh-sdk-protocol@0.1.0-rc.8
```

装完用入口探针确认可解析：`node -e "import(process.env.HOME + '/.dsh/profiles/<profile>/node_modules/@censorfs/deepseek-harness/src/index.js').then(() => console.log('ENTRY_IMPORT_OK'))"`。

### 运行环境变量（参考）

`deploy-explore.sh` 会写一份 `censorfs-explore.env`（`source` 它即可）；手动部署时的等价最小集：

```bash
export CENSORFS_SOCKET=/run/censorfs/control.sock     # CensorFS daemon socket
export CENSORFS_COMMAND=/usr/local/bin/censorfs
export CENSORFS_MOUNTER=/usr/local/bin/censorfs-mounter
export DSH_CENSORFS_CHILD_COMMAND=/usr/local/bin/dsh-jsonrpc-agent
```

`bin/dsh-jsonrpc-agent` 是 phase-1 POSIX sh 一次性适配器，不是正式的 JSON-RPC session 二进制；事件和反向控制由 `@censorfs/event-exporter` 负责。对外发布版本继续保留这一明确边界，正式协议实现属于后续 phase。注意经 sudo `env_reset` 后大部分 env 仍会被抹掉——模型、provider、event-socket 走 **argv** 透传（见 `bin/dsh-jsonrpc-agent`）。子 Harness 配置、Session persistence、密钥和模型日志必须位于 CensorFS workspace 外。

启动 daemon 和 Web Harness 后，在聊天输入：

```text
/explore 3 修复支付重复扣款问题，尽量保持改动小；使用 code validation profile
```

也可以运行 `demos/run-demo.sh` 准备三个精选工程。

## 直接命令

这些命令由竞技场按钮调用，也可人工输入：

```text
/censorfs-preview <run-id> <variant-id>
/censorfs-publish <run-id> <variant-id>
/censorfs-publish <run-id> <variant-id> --force
/censorfs-abort  <run-id> [variant-id]
/censorfs-policy <run-id> <variant-id> <directive>
/branch-graph    [run-id] [--tree]
```

- `--force` 只用于必需验证失败的 Candidate，并要求 UI 二次确认。它不绕过 Head CAS。
- `/censorfs-policy` 向运行中的 variant 发策略指令（`agent.steer`），不重启进程、不调模型。
- `/censorfs-abort` 优先走 socket 优雅取消；子进程 3s 内不退则 SIGTERM 硬杀。

## 配置约束

- `branch` 默认为 `main`，它是项目事实来源；导入目录只是初始化种子，发布不反向改写导入目录。
- `validationProfiles` 的命令与参数始终以 argv 数组执行，不拼接 shell 字符串。
- 探索工具的 `validationProfile` 是可选的；缺省时用 `defaultValidationProfile`，两者都未设则跳过验证（变体记为 `unvalidated`）。
- `childCommand` 和 validator 命令在 mounter 降权后执行，当前工作目录是 `/workspace`。
- 每个 Candidate validator 都使用只读 View；试图写源树应失败。需要写入的 build/cache/temp 必须指向 `$TMPDIR`。
- MVP 不含在线 GC。未采用 Candidate 会逻辑 Abort，但对象空间需要监控。

## 验证

不依赖 Harness 安装的纯单元测试：

```bash
cd integrations/deepseek-harness
npm test
```

底层组装测试：

```bash
cargo test --workspace --all-targets --locked
```

openEuler 真机还需运行：

```bash
bash scripts/openeuler-real-smoke.sh
bash scripts/openeuler-multiview-smoke.sh
bash scripts/openeuler-parallel-worlds-smoke.sh
```

发布前还必须在目标机运行完整 Runner 验收：

```bash
bash scripts/openeuler-namespace-runner-smoke.sh
bash scripts/openeuler-namespace-runner-cgroup-fault-smoke.sh
bash scripts/openeuler-dsh-inprocess-e2e.sh
DSH_REAL_E2E_COMMAND='dsh --profile web <host-specific-real-e2e-invocation>' \
DSH_SESSION_SNAPSHOT=/var/tmp/censorfs-release/session-events.json \
  bash scripts/openeuler-dsh-inprocess-real-e2e.sh
```

最后一个脚本不会猜测 DSH CLI、凭据或 Session 导出方式；调用方必须提供真实模型命令和 JSON 事件快照。它会验证 2–4 个 Variant 已完成 `Compare → Publish`，并保留快照作为发布证据。发布清单见 `docs/RELEASE_ACCEPTANCE.md`。

当前 Windows 开发环境只能验证 Rust 控制面、事件折叠和 JSON 契约；真实 FUSE、mount namespace、AArch64 以及完整 DeepSeek 模型调用必须在 openEuler/Linux 目标机验收。Windows 不提供 process-only 回退：`branch_explore_inprocess` 会因缺少 bwrap/FUSE 前置条件而 fail closed。

`branch_explore_inprocess` 会为每个 Variant 注册实时 `variant-activity` 投影，展示 read/write/edit/glob/grep/bash 时间线。配置 `maxTokens` 时，它作为本次探索总输出 token 预算，按 Variant 数量均分并传给每个 in-process Agent，确保并行探索不会超过总预算；因此该值必须至少等于 Variant 数量。

in-process Runner 目前只支持 `inProcessMaxDepth: 0|1`；嵌套 Runner 的 cgroup 归属与回收尚未实现，配置更大深度会在启动时拒绝。`/censorfs-policy` 和 Runner 内 `subagent` 工具仅支持完整 FUSE child 路径；对 in-process 探索会明确 fail closed。需要运行中改向或嵌套 Agent 的演示必须使用 `branch_explore`。

## 两条路线的边界（刻意低耦合）

两条探索路线只共享数据面公共件，其余刻意拆开；`src/combined-runtime.js` 是唯一同时知道两者的模块，跨轨调用一律得到点名轨道的明确报错，而不是静默串到另一轨。

| 层 | FUSE 轨（`branch_explore`） | Runner 轨（`branch_explore_inprocess`） | 共享 |
| --- | --- | --- | --- |
| 执行体 | 完整 child Harness 进程（`bin/dsh-jsonrpc-agent`） | Host 内 Agent + 专属 `runner-process.js` | — |
| 隔离 | mount namespace + 降权属主 | mount namespace Runner（工具执行面） | CensorFS view/ticket/CAS |
| 运行时 | `src/runtime.js` + `fuse-provider.js` | `src/namespace-runtime.js` + `runner-*.js` | `censorfs-cli` / `validation` / `events` |
| 专属特性 | preview、`/censorfs-policy`、子代理图/Git Graph、event-exporter | doctor、isolationSnapshot、Runner tool proxy | 竞技场命令（publish/abort/validate 按运行模式路由） |

**单轨部署开关**（互斥；默认双轨全启）：

```bash
export CENSORFS_INPROCESS_ONLY=1   # 只启用 Runner 轨：不建 FUSE 运行时、不要求 childCommand、不注册 branch_explore 工具
export CENSORFS_FUSE_ONLY=1        # 只启用 FUSE 轨：不建 RunnerManager、不做环境探测、不注册 branch_explore_inprocess 工具
```

配置键归属见 `cordis.patch.yml` 头部注释；`/explore` 默认走 **FUSE 轨**（`fuse`），仅在 FUSE 轨被禁用（`CENSORFS_INPROCESS_ONLY=1`）时才回落 `inprocess`；显式选择被禁用轨道会在命令层被拦截。

**收敛删除清单**（未来收敛到单轨时按表删除，数据面与公共件保留）：

| 收敛方向 | 删除 | 保留 |
| --- | --- | --- |
| 收敛到 FUSE（explore） | `src/namespace-runtime.js`、`runner-{manager,process,environment,tool-proxy}.js`、`in-process-provider.js`、`test/runner-*.test.js`、`test/runner-tool-proxy.test.js`、cordis.patch.yml Runner 段、`registerInProcessTool` 注册块 | `runtime.js`、`fuse-provider.js`、`bin/dsh-jsonrpc-agent`、event-exporter、共享公共件 |
| 收敛到 Runner | `src/runtime.js`、`fuse-provider.js`、`bin/dsh-jsonrpc-agent`、`event-exporter/`、cordis.patch.yml FUSE 段 | `namespace-runtime.js`、`runner-*.js`、共享公共件 |
| 旧架构残留（已删除） | ~~`src/in-process-runtime.js`、`src/in-process-path-rewrite.js`、`test/in-process-routing.test.js`、`test/in-process-path-rewrite.test.js`~~ 已随合并移除：旧"宿主内子代理 + 工具参数路径重写"架构被 Runner 轨取代，合并后无任何 import（git 历史可找回） | — |

## 进程内 Namespace Runner

`branch_explore_inprocess` 创建 Host 内 child Agent，并在首个 prompt 前启动、握手和绑定专属 `censorfs-mounter → runner-process.js`。`read/write/edit/glob/grep/read_image`、前后台 `bash` 与 `job_list/job_output/job_kill` 经 Runner 在对应 View 的真实 `/workspace` 执行；未知或未适配的 Host 本地工具 fail closed。完整 child Harness 的 `branch_explore`、event-exporter 实时事件流、cancel/update-policy 和 Git Graph UI 保持不变。

新部署默认 `runnerIsolation.mode=auto`、`minimumLevel=process`。可配置：

```yaml
runnerIsolation:
  mode: required        # auto | required | process | external
  minimumLevel: lifecycle  # process | lifecycle | resource
  root: /sys/fs/cgroup/censorfs-runners
  stateDir: /run/censorfs/cgroup-state
  memoryMax: '2147483648'
  pidsMax: '256'
  cpuMax: '200000 100000'
```

- `process`：Runner + bubblewrap + process-group best effort。
- `lifecycle`：增加 delegated cgroup v2 后代清理。
- `resource`：还要求 memory/pids/cpu 三项限额和 controller 全部可用。
- `required` 低于最低等级时 fail closed；`auto` 会报告降级。若 cgroup launch/readiness 在运行期失败，`auto` 清理失败 Runner、记录 `isolation-fallback` 并以 process 重试一次。
- `inProcessOnly: true` 时完整 child 路径不可用，`childCommand/provider/model` 对纯 in-process 部署可省略。旧 `runnerCgroup` 配置继续兼容。

`/censorfs-doctor` 支持 `--json`：输出单个机器可读 JSON 对象，`healthy: true` 且退出码 `0` 表示所有必要检查通过；退出码 `1` 表示未就绪或 fail closed。它不会启动或修改任何服务。Runner 可配置 `runnerAuditDir`（或环境变量 `CENSORFS_RUNNER_AUDIT_DIR`，必须位于 workspace 外），生命周期审计以不含凭据的 JSONL 写入该目录。排障步骤见 `TROUBLESHOOTING.md`，安全边界见 `SECURITY.md`。

```text
/censorfs-doctor
```

它检查已有 daemon socket、`censorfs`、`censorfs-mounter`、Node、`bwrap`、`/dev/fuse` 和 configured delegated cgroup v2；不启动 daemon、不创建 cgroup、不写 sudoers、不修改 systemd 或内核配置。使用 `/censorfs-doctor --json` 可供监控系统读取；Runner 审计可通过 `CENSORFS_RUNNER_AUDIT_DIR` 写入脱敏 JSONL。常见故障见 `TROUBLESHOOTING.md`，安全边界见 `SECURITY.md`。

`/explore` 缺省走 `branch_explore`（FUSE 轨，完整 child Harness 进程 + mount namespace）；需要成本更低的进程内档（experimental，尚未真机验证）时用 `/explore --mode inprocess <task>` 或明确要求 Agent 调用 `branch_explore_inprocess`。`CENSORFS_INPROCESS_ONLY=1` 时仅保留 in-process 路径。

TEST-ONLY 验证脚本：

```bash
bash scripts/openeuler-namespace-runner-smoke.sh
bash scripts/openeuler-namespace-runner-cgroup-fault-smoke.sh
bash scripts/openeuler-dsh-inprocess-e2e.sh
```

这些脚本只能使用独立测试 store/socket/cgroup root；生产实例只运行 `/censorfs-doctor`。详细设计与边界见 `docs/NAMESPACE_RUNNER_DESIGN.zh-CN.md`。
