# CensorScope

CensorScope 是被动式主机可观测性系统（version 0.1.1）：通过 procfs、eBPF tracepoint 与运行时动态 uprobe 采集进程、文件、网络、IPC、stdio 与应用负载观测，并把观测投影为可查询事件、负载分段与语义动作，落库 SQLite 供只读消费。

daemon 负责采集和数据库写入，其它程序通过文档化的只读视图或导出命令读取数据。

## 特性

- **采集等级（--level）**：能力集合按等级 L1/L2/L3 选择，默认 L1；等级通过启动参数设置。
- **TLS 明文被动采集**：运行时 uprobe 捕获 HTTPS 明文并按块落库（`payload_segments`），支持丢失审计重建；保留预算可配置。
- **Tool Call 级调用归因(需插件配合)**：从进程环境识别会话与工具调用，事件可精确归属到会话/调用。
- **dsh 集成**：仓库自带三个 dsh 插件（host / ui / agentcensor-session-proxy），一条脚本安装。
- **只读数据面**：SQLite `*_read` 视图与 `censorscopectl export` 支持查询和导出已落库数据。

## 采集等级

能力只由 `--level` 决定；未指定时默认 **L1**。各等级其余采集参数与 operator 配置默认一致。

| 等级 | 采集内容 |
|---|---|
| `L1`（默认） | 进程基础（fork/exec/exit/signal + argv/executable）与文件基本操作（open/close/dup/read/write/路径） |
| `L2` | L1 + mmap、网络端点、TLS 明文负载 |
| `L3` | 全量：L2 + IPC（pipe/socketpair，作为审计事件输出）与命令 stdout/stderr 正文 |

## 架构

```
crates/
├── apps/            # daemon（censorscoped）+ ctl（censorscopectl）
├── adapters/        # eBPF collector、UDS 控制面
├── contracts/       # 采集器/控制面/语义动作/进程树等跨层契约
├── core/            # 配置、模型、ingest/trace/semantic/process 运行时
├── storage/         # storage 抽象 + sqlite 适配
plugins/             # dsh 插件（censorscope-host / censorscope-ui / agentcensor-session-proxy）
scripts/             # install-censorscope.sh（dsh 插件安装）
```

## 快速开始

依赖：Linux、Rust 1.90+、clang/libbpf（构建 eBPF）；eBPF 采集需要 root 与 BTF，daemon 在受限环境下使用进程树快照。

```bash
# 构建
cargo build --release -p daemon -p ctl

# 生成默认 operator 配置（首次）
sudo ./target/release/censorscoped init

# 启动 daemon（前台调试用 run；后台用 start）
sudo ./target/release/censorscoped start                 # 默认等级 L1
sudo ./target/release/censorscoped start --level L3      # 指定等级（run/start/restart 可用）

# 开始观测
./target/release/censorscopectl track-add --root-pid xxxx

# 状态与控制
./target/release/censorscopectl doctor
./target/release/censorscopectl trace-list
./target/release/censorscopectl --json export --out-path trace.json
sudo ./target/release/censorscoped stop
```

与 CensorGuard 同时运行时，`censorscoped` 在 Guard 受管进程树外启动，以获得 eBPF 采集权限。
Guard 的管控规则对受管树内的 `bpf()` 和跨进程 ptrace 操作同样适用于 root 与 `censorguard` 组用户。
DSH host 插件 0.1.2 会显式传递自身 PID namespace，避免 ctl 子进程读取宿主
`/proc/PID/ns/pid` 时触发 ptrace 限制，需要同步重建安装 `censorscopectl`。

手动从受管 shell 观测自身时，可传入确切坐标：

```bash
pid_namespace=$(readlink /proc/self/ns/pid)
./target/release/censorscopectl track-add \
  --root-pid "$$" --root-pid-namespace "$pid_namespace"
```

显式 namespace 时，`--root-pid` 是该 namespace 中的 PID，客户端直接传递这组进程坐标。
Scope daemon 负责解析目标并校验 socket peer 的 UID、namespace 和授权。

## 配置

默认 operator 配置：`/etc/censorscope/censorscoped.conf`（`censorscoped init` 生成），TOML 段：

- `[daemon]` 路径/容量/超时；`[storage]` SQLite 路径；
- `[ebpf]` map/ring/TLS 相关；`[payload]` 负载保留预算；`[writer]` 落盘批处理
- `[profile]` 名称标签（运行能力由 `--level` 决定）

## 会话与调用归因

被观测进程通过环境变量关联会话与工具调用，内核与用户态按以下名称识别（写进子进程环境即可生效）：

- 会话：`CENSORSCOPE_SESSION_ID`（主），`DSH_SESSION_ID`（兜底，与 `[session] env_name` 一致）
- 工具调用：`DSH_CENSORSCOPE_CALL_ID`

缺少环境变量的事件按调用 span 时间窗做唯一回补；存在多个候选时保留原始归因状态。

## dsh 集成

三个插件均为 dsh profile 级安装，详见 [`plugins/README.md`](plugins/README.md)：

| 包 | 作用 | 装入 profile |
|---|---|---|
| `censorscope-host` | main：单 trace track-add/复用 + `/censorscope/call` 路由；worker：注入 `DSH_CENSORSCOPE_CALL_ID` + 上报 call span | web + headless |
| `censorscope-ui` | 浏览器会话「CensorScope」Tab（工具调用级观测视图） | web |
| `agentcensor-session-proxy` | web 每会话 headless worker + `CENSORSCOPE_SESSION_ID` | web |

`agentcensor-session-proxy` 为每个 Web 会话启动独立 worker，通过进程环境中的
`CENSORSCOPE_SESSION_ID` 关联该 worker 内执行的文件读写与子进程操作。
宿主同步根会话和子代理会话事件，浏览器通过 CensorScope 轨迹页查看工具调用及观测数据。
`censorscope-ui` 的 profile patch 将轨迹入口切换为 CensorScope 页面。

安装（插件变更需重新打包安装，脚本按版本号刷新）：

```bash
export DSH_HOME=<你的 dsh home>   # dsh 配置目录，通常为 ~/.dsh
./scripts/install-censorscope.sh all    # 安装 host、ui 和 proxy；host 模式安装 host 和 proxy
```

## 数据访问

daemon 独占 SQLite 写入；读取两条路径：

- **只读 SQL 视图**：`events_read`、`sessions_read`、`semantic_actions_read`、`semantic_action_links_read`、`semantic_action_evidence_read`、`semantic_contents_read`
- **导出命令**：`censorscopectl export --trace-id <id> [--session-id ..] [--call-id ..] [--full] [--out-path ..]`（`--full` 含 payload 十六进制；默认 lite）

## 开发与测试

```bash
cargo check --workspace --all-targets
cargo test --workspace --lib --bins
node --check plugins/*/lib/*.mjs plugins/censorscope-ui/lib/client.js   # 插件语法
```

## 致谢

本项目 fork 自 [openEuler/AcTrail](https://gitcode.com/openeuler/AcTrail)。感谢 AcTrail 团队和原始贡献者所做的出色工作。


## 许可

Mulan Permissive Software License，Version 2（MulanPSL-2.0），见 [LICENSE](LICENSE)。
