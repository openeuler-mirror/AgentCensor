# AgentCensor 安装与编排验收（2026-10-13）

本次在真实 Linux 主机上执行源码安装、systemd 启动、FUSE/BPF 批次、已安装 DSH 插件
和 Chromium 浏览器测试。结果不是“全部符合系统框图”：原生 Pivot 批次通过，DSH
普通对话目前只完成组件插件组合，尚未进入 Pivot 事务入口。

## 结果

| 检查 | 结果 | 实际证据 |
|---|---|---|
| README `install-agentcensor.sh all` | 通过 | 四个项目完成 release 构建与系统安装，三个数据面可启动 |
| 当前主机直接执行 README `start` | 环境条件未满足 | 宿主默认 cgroup v1，安装器明确拒绝，不冒充默认启动成功 |
| 隔离 cgroup v2 环境中的完整启动 | 通过 | 三组件 doctor 和 Pivot doctor ready，真实 FUSE 工具运行 |
| 原生 Pivot 与 `flow.md` 单批次流程 | 通过 | View、runner 登记、track-add、工具、Prepare、Commit/Abort 均实际运行 |
| Guard 0.5 接口 | 通过 | runner 在 Ready 前完成 `register_self`；FILE/EXEC 测试得到真实 EPERM 和对应 DENY |
| 组合配置实际加载 | 通过 | supervisor 与各 daemon 启动参数、Guard config_path、Scope 采集容量对应安装配置 |
| 配置不一致时拒绝启动 | 通过 | 改变 Pivot 的 FS socket 后，真实 serve 在监听前报 `differs from supervisor` |
| Scope 短批次采集与归属 | 修复后通过 | 每个执行过的工具均有内核事件和 call_id，调用跨度均已闭合 |
| 已安装 DSH Web 插件 | 修复后通过 | 对话、CensorScope、安全审计、策略设置、侧边栏预览及多轮会话通过 |
| DSH 对话自动走完整 Pivot 事务 | 未实现 | 普通 read/bash 测试前后没有新增 Pivot 事务记录 |

## 实际安装与启动

在仓库根目录执行了 README 的统一命令：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh all
```

它依次执行 `fs`、`guard`、`scope`、`pivot` 的构建安装。最初 sudo 选到了发行版
Cargo 1.82，无法构建 Guard 的 edition 2024；已修复安装器按项目固定工具链选择 Rust。
本次 FS 使用匹配其 pin 的系统 Rust 1.82，Guard、Scope、Pivot 使用 Rust 1.96.0。
原有 `/etc/censorguard/base.yaml` 等文件得到保留。

直接执行 `install-agentcensor.sh start` 时，主机没有
`/sys/fs/cgroup/cgroup.controllers`，安装器正确拒绝。为了继续验收，仅给测试服务提供
独立挂载的 cgroup v2，并通过临时 systemd `BindPaths` 在服务 mount namespace 内可见。
没有覆盖宿主的 `/sys/fs/cgroup`，没有修改引导参数或重启主机。该挂载没有资源控制器，
本次验证的是 cgroup 生命周期和后代清理，不包含 CPU/内存资源限制。
生产启动仍需满足 [安装指南](INSTALL.zh-CN.md) 的 cgroup v2 条件。

运行期间实际进程关系为：

```text
censord run --config /etc/agentcensor/censord.json
├── censorfsd --storage-root /var/lib/censorfs/.censorfs
│             --socket /run/censorfs/control.sock
├── censorguardd launch --config /etc/censorguard/agentcensor.yaml
│   └── censorguard-grpc --config /etc/censorguard/agentcensor.yaml
└── censorscoped --config /etc/censorscope/censorscoped.conf --level L3 run

censorpivot serve --config /etc/censorpivot/config.json   [非 root 系统用户]
```

`censord.json` 组合三个组件的启动参数和配置路径。CensorFS 存储/socket 参数直接来自
该 JSON；Guard 读取引用的完整 YAML；Scope 读取引用的 operator 配置并应用启动等级。
Pivot 自己的 JSON 负责事务与适配器。新增 `supervisor_config` 让 Pivot 在启动和 doctor
时校验这两份 JSON 的组件连接关系，避免各自连接到不同实例。

## 原生批次证据

测试入口为 [test-pivot-runtime.py](../scripts/test-pivot-runtime.py)。它使用已安装 daemon
和实际的 `censorpivot` 用户，临时下发 fixture 规则，结束后恢复原来的组策略。

```bash
sudo python3 CensorPivot/scripts/test-pivot-runtime.py /tmp/pivot-acceptance
```

脚本需要 Python 3.11+。成功测试会提交一个 fixture 文件并使 main Head 前进一步；拒绝
测试的私有写入会 Abort，Head 不前进。验证内容包括：

- `/workspace` 来自真实 `fuse.censorfs`，工具 UID 非零，两个工具看到同一个私有 View。
- 写入和读取全部成功后 Commit，CAS Head 只前进一步；同 request 重放不重复发布。
- FILE 读取拒绝和 EXEC 启动拒绝均实际返回 EPERM，Guard 审计的 domain、kind、DENY 对应。
- 拒绝后 Abort、不运行后续工具、不发布私有改动，批次 cgroup 后代得到清理。
- Scope SQLite 中的内核事件按 trace/call 归属，执行过的工具均有事件和闭合调用跨度。

最后一次批次的实际 Scope 事件数：

| 批次 trace_id | 工具 | 带 call_id 的内核事件 |
|---|---|---:|
| 4000001（Commit） | write / verify | 120 / 40 |
| 4000002（FILE 拒绝、Abort） | private-write / blocked | 70 / 37 |
| 4000003（EXEC 拒绝、Abort） | private-write / blocked | 70 / 20 |

事件数只是本次证据，不是固定数量的测试期望。脚本要求每个执行工具至少有事件。
Scope 观测记录不替代 Guard 的安全判定；DENY 证据来自 Guard 审计。

README 的原生 Demo 也真实执行成功：

```bash
cd CensorPivot
CENSORPIVOT_DEMO_GUARD_GROUP=censorguard-dsh-default \
  scripts/run-atomic-code-change-demo.sh
```

本次输出 `demo committed: branch=main head_seq=5->6 trace_id=3000004`。

## DSH 实际插件与浏览器测试

使用 DSH `0.1.5-rc.2` 的真实源码目录安装组合包：

```bash
DSH_ROOT=/root/deepseek-harness CensorPivot/scripts/install-dsh-censorpivot.sh
cd /root/deepseek-harness
pnpm dsh plugin --profile web list
pnpm dsh --profile web
```

Web profile 激活 `@agentcensor/censorpivot` 聚合层，三个组件包是其运行依赖；旧的
独立 Guard bundle 已从激活列表移除。八个本地 tarball 已重建，并保存对应 SHA-256。

浏览器入口为 [test-dsh-composite-web.mjs](../scripts/test-dsh-composite-web.mjs)，使用
已安装 profiles 的副本和实际服务，启动真正的 Web 与 headless worker。模型响应由
本地测试服务生成，从浏览器发消息触发原生工具，不消耗外部模型 API。测试证明：

- Web 进程实际登记在 Guard roots，策略设置页可读取当前规则。
- 首条消息创建常驻 worker，浏览器收到流式对话，CensorScope 和实时审计标签可打开。
- 在 worker 创建后从浏览器切换权限，再执行 read、bash cat、bash rm，三个工具均返回
  EPERM；删除目标仍存在，浏览器出现对应 cat/rm DENY。
- Scope 数据库存在该会话的读取/删除事件，bash 调用分别归属 `pivot-probe-1/2`。
- 侧边栏可预览普通文件；后续对话复用同一个 worker，没有事件序号断档。

完整测试日志：`/tmp/agentcensor-acceptance-20261013/dsh-web-final.log`。
浏览器截图、页面快照、工具结果和宿主日志：`/tmp/pivot-web-CU0nsf/`。

### 与系统框图的差距

当前 DSH 聚合层的 `lib/index.mjs` 是 marker，组合 patch 激活原有插件，并没有把
普通 read/bash 转成 Pivot `BatchRequest`。这些调用由 Scope worker 内的原生 DSH
工具执行，继承 Guard domain；FS View 只由对应组件工具独立创建。

浏览器测试明确检查了 `/var/lib/censorpivot/transactions` 前后相同。因此只能确认
“DSH 三组件插件正常组合运行”，不能确认“DSH 普通对话严格执行 flow 的完整事务”。
要满足后一要求，需要增加 DSH 工具到 Pivot 的实际事务适配，包括 View 工作目录、
工具调用表示、批次边界、Commit/Abort 和取消语义；修改文档不能代替这个实现。

## 本次修复与回归

1. 安装器正确选择 Rust 工具链，源码启动命令改为 DSH 实际支持的 `--profile web`。
2. Guard 0.5 迁移共用配置，可信 runner 在接受工具前通过新接入登记实际 peer PID。
3. Scope 允许同 UID、同 PID namespace 的可信 coordinator 后代跨私有 mount namespace
   登记；拒绝无关进程、跨 UID/PID namespace 与 root 容器的该种委托，加入边界测试。
4. Scope 移除 trace 前排空已采集事件并等待数据库提交，避免短批次丢归属。
5. Scope 新建 daemon-lifetime spool 时同步重置数据库去重 checkpoint，修复重启后新
   事件被当作旧序号跳过的问题；保留历史数据和同一 spool 内的重放去重。
6. DSH 正确识别绝对路径的 tsx ESM 入口，使用与宿主一致的 scope 模块。
7. 宿主权限/沙箱/审批/模型事件同步到常驻 worker，以独立 worker 游标避免 UI 事件插入
   导致断档；实际多轮浏览器测试覆盖此问题。
8. Pivot 启动校验组合配置一致性，实际错误配置测试证明会在监听前拒绝。

Pivot：36 个测试通过，fmt、Clippy `-D warnings` 和 release 构建通过。
Scope：daemon 58 个库测试、5 个二进制测试、SQLite 24 个测试通过；特权/性能 opt-in
测试未作为普通单测运行。Scope `--no-deps` Clippy 仍有四个既有告警
（type_complexity、两个 collapsible_if、or_insert_with），不能声称整个 Scope lint 全通过。
组合插件打包、当前 DSH CLI profile 检查和最终真实浏览器测试通过。

完整日志和 JSON 证据保存于 `/tmp/agentcensor-acceptance-20261013/`。本次结束后停止
测试服务，撤销测试创建的统一服务自启动、临时 systemd drop-in 和 cgroup v2 挂载。
已安装组件、配置、组合插件和验收证据保留；现有 Guard 策略状态未整体清空。
