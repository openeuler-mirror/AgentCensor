# CensorScope 命令手册

> 本手册内容完全依据 CensorScope 仓库源码整理（主要为 `crates/apps/daemon` 与 `crates/apps/ctl`），未参考任何文档说明。
>
> 项目提供两个命令行二进制：
> - **censorscoped** — 守护进程本体及进程监督（`crates/apps/daemon/src/bin/censorscoped.rs`）
> - **censorscopectl** — 控制面客户端 / 本地工具（`crates/apps/ctl/src/bin/censorscopectl.rs`）

---

## 一、censorscoped（守护进程）

源码：`crates/apps/daemon/src/bin/censorscoped/args.rs`、`entry.rs`、`process.rs`

clap 描述：`Run and supervise the CensorScope daemon`

### 全局参数

| 参数 | 说明 |
| --- | --- |
| `--config <PATH>` | 运营配置文件路径，默认 `/etc/censorscope/censorscoped.conf`（`DEFAULT_OPERATOR_CONFIG_PATH`） |
| `--level <LEVEL>` | 采集级别，仅允许 `L1` / `L2` / `L3`，默认 `L1`。**仅对 `run` / `start` / `restart` 有效**；用于 `init` / `stop` / `status` 会直接报错：`--level is only valid with run, start, or restart`。级别是单次运行状态，绝不写入配置文件，也不会在运行中切换 |

采集级别能力集（`crates/core/config/src/capture_profile.rs`，逐级为超集）：

| 级别 | 能力 |
| --- | --- |
| `L1` | 进程生命周期、进程 exec 上下文、基础文件操作（默认） |
| `L2` | L1 + mmap、网络端点、TLS 明文载荷 |
| `L3` | L2 + IPC 管道/FIFO、IPC Unix socket、stdio 数据块（全量 9 项能力） |

### 子命令

#### 1. `censorscoped init [--output PATH] [--patch PATH] [--force]`

初始化默认运营配置文件（TOML）。

- `--output` 与 `--config` 互斥：同时给出报错 `init accepts either --output or --config, not both`
- 目标文件已存在且未加 `--force`：只做校验，打印 `config <path> already exists and is valid`
- `--force`：覆盖重写，打印 `overwrote config <path>`；新建打印 `initialized config <path>`
- `--patch <PATH>`：在默认配置上合并一个 TOML 补丁后再落盘（表递归合并，其余直接覆盖）
- 写文件前会自动创建父目录

#### 2. `censorscoped run`

前台运行守护进程。

- 安装 SIGTERM / SIGINT 优雅退出处理器
- 启动时写 PID 文件（`create_new` 独占创建，PID 为 0 拒绝）
- eBPF 不可用（`enabled = "auto"` 探测失败）时降级为 snapshot-only 跟踪并打印 warn 日志
- 就绪后打印：`daemon listening socket=<sock> storage=<db>`
- 退出时清理 PID 文件与控制 socket

#### 3. `censorscoped start`

后台（分离）启动守护进程。

- 前置检查：已运行 / 残留 PID 文件 / 残留 socket 都会报错并提示人工删除
- 通过 `setsid` 派生子进程执行 `run --config <path> --level <level>`，stdout/stderr 追加到 `log_path`
- 在 `startup_wait_ms`（默认 5000ms）内以 `supervision_poll_interval_ms`（默认 50ms）轮询，等待 PID 文件与 socket 同时出现
- 成功打印：`censorscoped started pid=<pid> socket=<sock>`
- 超时或子进程提前退出：先 SIGTERM、超时后强杀，清理运行时文件，并报告 `startup_child=already-exited|terminated-after-startup-failure|force-stopped-after-startup-failure`

#### 4. `censorscoped stop`

停止守护进程。

- 已停止：打印 `censorscoped already stopped`
- 运行中：向 PID 文件记录的进程发 SIGTERM，在 `shutdown_wait_ms`（默认 10000ms）内等待退出，随后删除 PID 文件与 socket，打印 `censorscoped stopped pid=<pid>`
- 残留 PID 文件 / 残留 socket：报错并要求确认后手动删除

#### 5. `censorscoped restart`

等价于先 `stop` 再 `start`（使用同一 `--level`）。

#### 6. `censorscoped status`

查询运行状态，输出四种之一：

```
censorscoped running pid=<pid>
censorscoped stopped
censorscoped stale pid=<pid>
censorscoped stale socket=<path>
```

判定依据：PID 文件存在且进程存活 → running；PID 文件存在但进程不在 → stale pid；无 PID 文件但 socket 存在 → stale socket；都没有 → stopped。

---

## 二、censorscopectl（控制客户端）

源码：`crates/apps/ctl/src/args.rs`、`entry.rs`、`dispatch.rs`、`output.rs`、`export.rs`

clap 描述：`Control a running CensorScope daemon`

### 全局参数

| 参数 | 说明 |
| --- | --- |
| `--config <PATH>` | 运营配置路径，默认 `/etc/censorscope/censorscoped.conf`。除 `init` 外的命令都会加载它（`export` 仅从其中取 `storage.path`） |
| `--socket-path <PATH>` | 控制 socket 路径，覆盖配置中的 `daemon.socket_path`；远程命令最终必须能确定一个 socket 路径，否则报错 `missing --socket-path and operator config was not loaded` |
| `--request-id <ID>` | u64 请求 ID；缺省用当前时间（纳秒）生成 |
| `--json` | 以 JSON 打印控制回执；出错时打印 `{"ok":false,"code":...,"message":...}` 并以退出码 1 结束 |

### 子命令

#### 1. `censorscopectl init [--output PATH] [--patch PATH] [--force]`

本地命令（不连守护进程），语义与 `censorscoped init` 相同。`--json` 输出：

```json
{"ok": true, "status": "created|existing_valid|overwritten", "path": "..."}
```

#### 2. `censorscopectl track-add --root-pid <PID> [--name NAME] [--tag TAG]... [--trace-id ID]`

把一条 trace 挂到已存在的根进程上（发给守护进程）。

- `--root-pid`（必填）：根进程 PID。客户端会读取 `/proc/<pid>/status` 的 `NSpid` 末列与 `/proc/<pid>/ns/pid` 链接，组成调用方命名空间视角的进程引用
- `--name`：显示名，缺省为 `pid-<root-pid>`；空串报错
- `--tag`：可重复，去重后作为标签集合
- `--trace-id`：u64，传入表示沿用已有 trace id，否则由守护进程分配
- 采集 profile 名取自运营配置的 capture profile
- 成功输出：`trace <id> entered <lifecycle_state>`；`--json`：`{"ok":true,"trace_id":N,"lifecycle_state":"..."}`

#### 3. `censorscopectl track-remove <选择器>`

按选择器移除 trace。**必须且只能给出一个**选择器：

- `--trace-id <ID>` / `--root-pid <PID>` / `--name <NAME>` / `--tag-selector <TAG>`
- 不给：`one selector flag is required: --trace-id, --root-pid, --name, or --tag-selector`
- 给多个：`selector flags are mutually exclusive`
- 成功输出：`root capture removed`

#### 4. `censorscopectl trace-list [选择器]`

列出 trace。选择器参数与 `track-remove` 相同，但全部可选（至多一个；不给则列出全部）。

文本输出每行一条：

```
<trace_id> <display_name> pid=<root_pid> pidns=<命名空间|none> container=<容器id|none> <lifecycle_state>/<health>
```

`--json` 输出 `traces` 数组，含 `trace_id / display_name / root_pid / root_pid_namespace / root_container_id / lifecycle_state / health(clean|degraded) / tags / created_at`。

#### 5. `censorscopectl doctor`

检查守护进程控制面就绪情况。

- 文本输出：`collectors=<逗号分隔的可用采集器> storage_ready=<true|false>`
- `--json`：`{"ok":true,"available_collectors":[...],"storage_ready":bool}`

#### 6. `censorscopectl call-start --trace-id <ID> --call-id <ID> --pid <PID> [--session-id <S>] [--started-at <TS>]`

开启一个工具调用区间（span）。`--started-at` 缺省取当前时间。成功输出：`call span started`。

#### 7. `censorscopectl call-end --trace-id <ID> --call-id <ID> --pid <PID> [--session-id <S>] [--ended-at <TS>] [--status <S>]`

结束工具调用区间。`--ended-at` 缺省取当前时间；`--status` 缺省 `success`。成功输出：`call span ended`。

#### 8. `censorscopectl export --trace-id <ID> --session-id <S> --out-path <PATH> [选项]`

本地命令（不连守护进程）：以只读方式打开配置中 `storage.path` 指向的 SQLite 库，导出快照并原子写入（临时文件 + rename）为 pretty JSON。成功打印 `exported snapshot to <path>`。

| 选项 | 说明 |
| --- | --- |
| `--trace-id <ID>`（别名 `--trace`） | 目标 trace，u64 |
| `--session-id <S>` | 会话 ID，必填且非空；过滤 events / payload_segments / semantic_actions / sessions 等 |
| `--call-id <C>`（别名 `--call`） | 只保留该 call_id 的事件与载荷段 |
| `--out-path <PATH>` | 输出文件路径（父目录自动创建） |
| `--full` | full 模式：载荷段内嵌 `bytes_hex`、语义动作带完整 `attributes`、contents 带 `canonical_json`；缺省为 lite 模式（不含上述内容，只给 `attribute_count`） |
| `--no-internal` | 剔除插件自噪声：`tls.coverage` 诊断行与 `censorscopectl` 自身进程事件 |
| `--page-size <N>` | 每类数组截断到 N 条（N 必须 > 0） |
| `--after-event <N>` | 只保留 `event_id > N` 的事件（分页游标） |

输出 JSON 顶层字段：`schema_version(=1)`、`mode(full|lite)`、`trace_id`、`session_id`、`traces`、`processes`、`memberships`、`sessions`、`call_spans`、`events`、`payload_segments`、`diagnostics`、`actions`/`semantic_actions`、`links`、`evidence`、`contents`、`pagination{page_size,after_event,next_event}`、`truncated`。纳秒级时间戳以字符串形式输出；fd 类读写事件会根据 open/dup/close 记录回填文件路径。

### 错误与退出码

- 控制命令失败（非 `--json`）：`control command failed: <code>: <message>`
- `--json` 模式：打印 `{"ok":false,...}` 并返回退出码 1；成功返回 0

---

## 三、运营配置文件（censorscoped.conf）

两个二进制的 `--config` 均指向同一 TOML 文件，默认 `/etc/censorscope/censorscoped.conf`。schema 见 `crates/core/config/src/daemon.rs`（`deny_unknown_fields`，多写字段会被拒绝）。`init` 生成的默认值：

```toml
[daemon]
socket_path = "/run/censorscope/censorscoped.sock"
socket_mode = 438            # 0o666
control_pending_connection_max = 256
active_trace_max = 128
pid_file = "/run/censorscope/censorscoped.pid"
log_path = "/var/log/censorscope/censorscoped.log"
startup_wait_ms = 5000
shutdown_wait_ms = 10000
supervision_poll_interval_ms = 50

[storage]
backend = "sqlite"           # 只支持 sqlite
path = "/var/lib/censorscope/censorscope.sqlite"
busy_timeout_ms = 5000

[profile]
name = "process-lifecycle"   # 历史字段；运行期 profile 由 --level 决定

[ebpf]
enabled = "auto"             # true / false / auto
memlock_rlimit = "unlimited" # inherit / unlimited / bytes:<n>
tracked_process_max_entries = 262144
pending_operation_max_entries = 262144
event_ring_buffer_max_bytes = 67108864
tls_dynamic_loading = false
tls_scan_debounce = "image"  # none / image / interval
tls_scan_interval_ms = 250   # 必须 > 0

[payload]
max_trace_bytes = 67108864   # 0 表示不限
max_segment_bytes = 1048576  # 必须 > 0

[session]
env_name = "DSH_SESSION_ID"  # 标识被观测进程会话的环境变量名

[writer]
batch_items = 512            # 必须 > 0
idle_timeout_ms = 8          # 必须 > 0
checkpoint_interval_secs = 60 # 0 关闭周期 checkpoint（退出时仍做最终 checkpoint）
queue_cap_items = 16384
```

---

## 四、辅助脚本

`scripts/install-censorscope.sh [all|host]`（源码内唯一安装脚本）：

- `all`（默认）：向 dsh 的 `web`、`headless` 两个 profile 安装 `censorscope-host` 插件，并向 `web` 安装 `agentcensor-session-proxy` 与 `censorscope-ui`
- `host`：只装 `censorscope-host` 与 `agentcensor-session-proxy`，不装 UI
- 其他参数打印 `usage: install-censorscope.sh [all|host]` 并以退出码 2 结束
- 依赖 `DSH_HOME`（默认 `~/.dsh`）下已存在的 profile，通过 `npm pack` + `dsh plugin --profile <p> add <tgz>` 安装；版本一致则跳过

---

## 五、典型命令序列（按源码行为推导）

```bash
# 1. 生成默认配置（已存在则只校验）
censorscoped init            # 或 censorscopectl init

# 2. 启动守护进程（L2 采集级别）
censorscoped start --level L2
censorscoped status

# 3. 检查控制面
censorscopectl doctor

# 4. 跟踪一个现有进程树
censorscopectl track-add --root-pid 1234 --name demo --tag test --json

# 5. 查看 / 移除
censorscopectl trace-list
censorscopectl track-remove --trace-id <ID>

# 6. 记录一次工具调用区间
censorscopectl call-start --trace-id <ID> --call-id c1 --pid 1234
censorscopectl call-end   --trace-id <ID> --call-id c1 --pid 1234 --status success

# 7. 导出快照
censorscopectl export --trace-id <ID> --session-id <S> --out-path /tmp/snap.json --full

# 8. 停止守护进程
censorscoped stop
```
