# CensorScope modular benchmark

这里的测试按 CensorScope 的代码职责划分为 module，而不是按开发阶段划分。
每个 module 都会创建独立的 daemon、SQLite、spool、socket 和结果目录；长时间
运行、较大数据量和受控过载只是相应 module 的 workload 配置。

collector、ingest 和 storage 在 daemon 内部是串联链路，不能在不改变产品路径的
情况下完全拆成三个独立进程；对应 module 会保持真实链路，只改变 workload、采样
和判定指标，从而定位主要瓶颈而不制造另一套测试实现。

`preflight`、`collector`、`ingest`、`storage`、`control`、`export` 和 `memory`
默认直接启动 `workloads/system_load.py`，不依赖 dsh。只有 `e2e` 使用 DeepSeek
Harness 的 `dsh --profile headless`；runner 默认用 `shutil.which("dsh")` 找到它，
也可以用 `--dsh-path` 指定路径。

## 测试前提

目标机需要具备：

- Linux、release 构建的 CensorScope daemon/ctl；
- 运行 eBPF 所需权限、BTF 和 tracefs；
- 运行 e2e 时需要 Node.js、DeepSeek Harness、可工作的 dsh profile 和已安装的
  CensorScope dsh 插件；其它 module 不需要 dsh；
- Python 3.11 或更新版本。

构建并做 benchmark 自身检查：

```bash
cargo build --release -p daemon -p ctl
PYTHONPATH=benchmark python3 -m unittest discover -s benchmark/tests -v
```

只有运行 e2e 时才需要检查 dsh：

```bash
dsh --version
```

如果二进制不在默认位置：

```bash
python3 benchmark/run.py \
  --dsh-path /path/to/dsh \
  --daemon-path /path/to/censorscoped \
  --ctl-path /path/to/censorscopectl
```

## 运行入口

默认场景文件是 [`scenarios/deepseek-harness.toml`](scenarios/deepseek-harness.toml)。

运行全部 module：

```bash
python3 benchmark/run.py
```

由于全部 module 包含 `e2e`，上面的命令仍然需要 dsh。没有 dsh 时请显式选择
后端 module，或使用 `--mode native`。

默认情况下，后端 module 使用 native workload，只有 `e2e` 使用 dsh。只运行不依赖
dsh 的性能 module：

```bash
python3 benchmark/run.py --module preflight,collector,ingest,storage,control,export,memory
```

也可以显式指定 native 模式；该模式不能用于 `e2e`：

```bash
python3 benchmark/run.py --mode native --module collector,storage,memory
```

只运行一个 module：

```bash
python3 benchmark/run.py --module collector
```

运行多个 module：

```bash
python3 benchmark/run.py --module collector,storage,memory
```

只打印计划，不启动 daemon 或 dsh：

```bash
python3 benchmark/run.py --module e2e --dry-run
```

非 e2e module 的 native 模式会直接启动 runner 生成的 workload，并在启动前用
`track-add` 追踪 workload 根 PID。自动模式只对 e2e 生效，会让 dsh 使用 shell
tool 执行 workload。手动模式适用于需要人工处理 dsh 交互、审批或特殊 profile
的 e2e 测试：

```bash
python3 benchmark/run.py --module e2e --mode manual
```

runner 会打印命令并要求输入 dsh PID，然后执行同样的采样、停止和校验流程。

## Module 说明

下面每个 module 都说明四件事：测试对象、开始方式、结束条件和结果解读。

### `preflight`：环境和 daemon readiness

测试对象：

- daemon 配置生成；
- SQLite 初始化；
- Unix socket 控制面；
- `doctor` readiness；
- eBPF 是否可用或发生自动降级。

开始：

```bash
python3 benchmark/run.py --module preflight
```

runner 创建临时配置，启动前台 daemon，调用 `doctor`，等待 readiness，然后
停止 daemon。这个 module 不启动 dsh workload，也不要求产生事件。

结束：daemon 正常启动并被 runner 停止，或者在 30 秒 readiness 超时/启动错误时
失败。日志位于该 case 的 `runtime/censorscoped.log`。

解读：

- 失败表示测试环境或 daemon 启动条件不满足，不能解读其它 module 的性能；
- `doctor` 成功但日志出现 eBPF auto-degraded，说明后续测试不是完整 eBPF 采集；
- 目标性能测试应要求 collector 可用，而不是接受降级结果。

### `collector`：collector actor 和 eBPF 采集

测试对象：

- eBPF attach 和 ring buffer；
- collector actor 的 poll/raw batch；
- spool append；
- fork/exec/exit、文件、网络、IPC、stdio 等采集类别；
- collector drop 和 loss diagnostics。

开始：

```bash
python3 benchmark/run.py --module collector
```

runner 先启动 daemon，然后让一个暂停的 workload 根 PID 完成 `track-add`，再
继续同一个 PID。workload 会生成进程、
文件、mmap、pipe、socketpair、loopback 网络和子进程 stdout 活动。

结束：workload 写出 `workload/complete.json` 后，runner 等待 `drain_seconds`，
停止 dsh 和 daemon，读取数据库统计和日志。

解读：

- `stats.events` 反映最终落库事件量；
- `stats.loss_diagnostics`、collector 日志和 summary 中的损失字段反映丢失位置；
- `workload.totals.skipped_operations > 0` 表示测试权限不满足，case 直接失败；
- 该 module 的重点不是绝对 events/s，而是事件是否随 workload 增长、是否出现
  collector drop，以及内存是否持续增长。

### `ingest`：spool drain、解码、归一化和语义处理

测试对象：

- raw spool decode；
- collector batch 到 domain event 的转换；
- session/call 归因；
- payload segment 重组；
- HTTP/SSE/LLM/MCP 语义 action 生成；
- ingest 队列在较大 batch 下的积压和丢失。

开始：

```bash
python3 benchmark/run.py --module ingest
```

执行方式与 collector 类似，但 workload 的迭代次数、文件活动和 payload 更大，
用来让 ingest 处理路径持续忙于解码和解析。

结束：workload 完成并经过 drain window；daemon 停止后再读取 SQLite，避免只看
尚在 writer 队列中的数据。

解读：

- 对比 `workload.totals` 与 `stats.events`、`stats.payload_bytes`；
- semantic action 数量异常为零时，先检查 dsh 插件、TLS 采集和 payload 是否实际
  产生，不能直接归因于 ingest CPU；
- `data.queue_overflow`、`data.event_row_failure` 和 `data.payload_row_failure`
  是 ingest 到 writer 之间的损失信号。

### `storage`：writer、SQLite 事务和 WAL

测试对象：

- writer bulk queue 和 memory budget；
- `batch_items`、idle commit；
- SQLite 单写线程和批量事务；
- WAL 增长、checkpoint 和最终提交；
- event/payload/semantic row failure。

开始：

```bash
python3 benchmark/run.py --module storage
```

该 module 使用较大的 payload 和较多并发 worker。writer 参数来自场景的
`[censorscope.writer]`，可以复制场景文件后一次只修改一个参数进行对比。

结束：workload 完成后先等待 drain，再让 daemon 做最终 checkpoint；之后读取
`stats.database_bytes`、`stats.wal_bytes`、`stats.spool_bytes` 和失败计数。

解读：

- WAL 或 spool 持续增长而内存也持续增长，说明 storage 处理速度跟不上输入；
- queue overflow 但 RSS 稳定，表示系统按预算丢弃/上游吸收了压力，应结合损失账本
  判断是否可接受；
- 单次运行的数据库大小不是硬件性能结论，只用于比较同一场景参数的变化。

### `control`：UDS 控制面和控制优先级

测试对象：

- Unix socket accept/poll；
- control worker；
- writer 的独立 control lane；
- 持续 ingest 时的 `doctor` 和 `trace-list` 延迟。

开始：

```bash
python3 benchmark/run.py --module control
```

workload 运行期间，runner 每 200ms 轮换发送 `doctor` 和 `trace-list`，同时采集
请求耗时和错误。workload 完成后停止 probe，再执行 drain 和 shutdown。

结束：workload marker 出现、probe 停止、daemon 正常关闭。结果写入
`control_probe`：请求数、失败数、p50、p95 和最大耗时。

解读：

- `failures > 0` 表示控制面在采集压力下不可用；
- p95 大幅增长但没有 OOM，通常说明 writer commit、checkpoint 或控制 worker
  被拖慢；
- 不使用固定毫秒门槛跨硬件比较，只比较同一环境和配置的趋势。

### `export`：只读查询和导出

测试对象：

- `events_read`、payload 和 semantic action 查询；
- snapshot full 导出；
- OTLP JSON 导出；
- 分页和序列化的内存/耗时影响。

开始：

```bash
python3 benchmark/run.py --module export
```

先生成比正常运行更大的 trace，停止 daemon 确保数据提交完成，再运行 snapshot
和 OTLP export。导出文件放在 case 的 `exports/` 中。

结束：两个导出命令返回成功或超时/错误。结果包含每种格式的耗时和文件大小。

解读：

- `snapshot` 没有 session 时会标记为不可用；这通常表示 dsh 集成没有注入 session；
- OTLP 是 trace-only 导出，不依赖 snapshot 的 session filter；
- 导出耗时应结合事件数、payload bytes 和输出文件大小一起看，不能只看秒数。

### `memory`：长期内存稳定性

测试对象：

- daemon 进程树；
- dsh 及其 worker/tool 子进程；
- workload 子进程；
- SQLite、WAL 和 spool 文件随时间的变化。

开始：

```bash
python3 benchmark/run.py --module memory
```

默认 workload 持续 30 分钟。采样间隔、尾段比例和稳定阈值由 `[memory]` 控制。
运行期间不会重启 daemon，也不会执行 spool replay；spool 只属于当前 daemon
生命周期。

结束：workload duration 到期，runner 等待 drain，停止 dsh 和 daemon，并写入
`samples.csv`、最终数据库统计和 OOM 检测结果。memory module 不执行完整 OTLP
导出，因为长时间 workload 会产生超大 trace；导出由 `export` module 专门验证。

解读：

- `memory.peak_rss_bytes` 是观测峰值；
- `memory.tail_growth_ratio` 是尾段相对峰值的增长比例；
- `memory.tail_slope_bytes_per_second` 用于判断是否还有线性增长；
- `oom_detected = true` 或尾段超过 `max_tail_growth_ratio` 即失败；
- RSS 达到平台后有小幅抖动属于正常现象，持续向上才是内存稳定性问题。

### `e2e`：完整 dsh 用户链路

测试对象：

- dsh 启动和 profile；
- CensorScope host/session/call 插件；
- track-add 到子进程继承；
- collector、ingest、writer、SQLite；
- snapshot 和 OTLP export；
- 完整进程树内存。

开始：

```bash
python3 benchmark/run.py --module e2e
```

默认重复三次，每次使用新的 daemon 和数据库。dsh 的 shell tool 执行固定 workload，
runner 等待 `complete.json`，随后完成 drain、shutdown、数据库统计和导出。

结束：每个 case 必须完成 workload、提交事件、成功导出 OTLP，并通过 OOM 和尾段
内存判定；三次 case 的结果可以用于观察重复性。

解读：

- 这是用户视角的最终结论，不用于定位具体瓶颈；
- 如果 e2e 失败，应先运行 `preflight`，再按 collector、ingest、storage、control、
  export 顺序定位；
- `workload.log`（native）、`dsh.log`（e2e）、`runtime/censorscoped.log` 和
  `summary.json` 是首要诊断材料。

## 结果目录和判定

默认结果写入 `benchmark/results/`：

```text
results/
├── plan.json
├── summary.json
├── summary.md
└── storage-01/
    ├── summary.json
    ├── samples.csv
    ├── workload.log（native module）
    ├── dsh.log（e2e module）
    ├── runtime/censorscoped.log
    ├── workload/complete.json
    └── exports/
```

通过条件：

- workload 完成（`preflight` 除外）；
- 没有 OOM、异常 SIGKILL 或 daemon 启动失败；
- 非 preflight module 有实际落库事件；
- 需要导出的 module 的 OTLP 导出成功；memory module 不执行完整导出；
- IPC/网络 workload 没有因权限而跳过；
- memory module 的尾段增长不超过配置阈值。

collector drop、queue overflow、writer failure 不会被隐藏，也不会简单转换成
“性能通过”。它们会保留在 `summary.json`、SQLite diagnostics 和 daemon log 中，
用于区分“内存稳定但开始丢数据”和“内存持续增长/OOM”。

默认保留日志、配置和结果，清理数据库/WAL/spool。需要保留完整运行时文件：

```bash
python3 benchmark/run.py --module storage --keep-runtime
```

测试代码不使用硬件规格作为通过条件；如果目标机没有所需权限，测试应报告环境
失败，而不是把缺少 eBPF、网络或 IPC 权限解释为 CensorScope 性能结果。
