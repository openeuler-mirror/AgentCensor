# CensorScope

CensorScope 是被动式主机可观测性系统（version 0.1.1）：在不修改、不要求被观测程序配合的前提下，通过 procfs、eBPF tracepoint 与运行时动态 uprobe 采集进程、文件、网络、IPC、stdio 与应用负载观测，并把观测投影为可查询事件、负载分段与语义动作，落库 SQLite 供只读消费。

CensorScope 不执行文件、网络或命令策略；没有 agent SDK、preload、插件运行时或主动上报协议。daemon 独占数据库，其它程序通过文档化的只读视图或导出命令读取数据。

## 特性

- **采集等级（--level）**：能力集合按等级 L1/L2/L3 选择，默认 L1；等级是本次运行属性，不写入配置文件。
- **TLS 明文被动采集**：运行时 uprobe 捕获 HTTPS 明文并按块落库（`payload_segments`），支持丢失审计重建；保留预算可配置。
- **Tool Call 级调用归因(需插件配合)**：从进程环境识别会话与工具调用，事件可精确归属到会话/调用。
- **dsh 集成**：仓库自带三个 dsh 插件（host / ui / agentcensor-session-proxy），一条脚本安装。
- **只读数据面**：SQLite `*_read` 视图与 `censorscopectl export` 供查询/导出，不依赖 daemon 在线。

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

依赖：Linux、Rust 1.90+、clang/libbpf（构建 eBPF）；eBPF 采集需要 root 与 BTF，不可用时 daemon 自动降级为进程树快照。

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

## 配置

默认 operator 配置：`/etc/censorscope/censorscoped.conf`（`censorscoped init` 生成），TOML 段：

- `[daemon]` 路径/容量/超时；`[storage]` SQLite 路径；
- `[ebpf]` map/ring/TLS 相关；`[payload]` 负载保留预算；`[writer]` 落盘批处理
- `[profile]` 名称标签（运行能力由 `--level` 决定）

## 会话与调用归因

被观测进程通过环境变量关联会话与工具调用，内核与用户态按以下名称识别（写进子进程环境即可生效）：

- 会话：`CENSORSCOPE_SESSION_ID`（主），`DSH_SESSION_ID`（兜底，与 `[session] env_name` 一致）
- 工具调用：`DSH_CENSORSCOPE_CALL_ID`

无环境变量的事件按调用 span 时间窗做唯一回补，不确定时保持未归因。

## dsh 集成

三个插件均为 dsh profile 级安装，详见 [`plugins/README.md`](plugins/README.md)：

| 包 | 作用 | 装入 profile |
|---|---|---|
| `censorscope-host` | main：单 trace track-add/复用 + `/censorscope/call` 路由；worker：注入 `DSH_CENSORSCOPE_CALL_ID` + 上报 call span | web + headless |
| `censorscope-ui` | 浏览器会话「CensorScope」Tab（工具调用级观测视图） | web |
| `agentcensor-session-proxy` | web 每会话 headless worker + `CENSORSCOPE_SESSION_ID` | web |

安装（插件变更需重新打包安装，脚本按版本号刷新）：

```bash
export DSH_HOME=<你的 dsh home>   # 不设置则默认 ~/.dsh，建议设置DSH_HOME
./scripts/install-censorscope.sh all    # host+ui+proxy；host 模式只装 host
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
