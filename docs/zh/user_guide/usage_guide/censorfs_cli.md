# CensorFS 命令使用手册

`censorfs` 是面向功能演示和运维检查的多调用程序。下面两种写法完全等价：

```bash
censorfs ls --branch main /
censorfs-ls --branch main /
```

所有 `censorfs-*` 名称都是指向同一个 `censorfs` 二进制的符号链接，不会复制业务逻辑。文件命令通过 Unix Socket 直接操作 daemon 内的 `ViewEngine`，不会创建 FUSE mount；真实 Agent `/workspace` 的测试方式见 [openEuler 鲲鹏构建与部署](../../../../CensorFs/docs/OPENEULER_AARCH64.md)。

## 1. 启动顺序

一个实例涉及三个路径：

| 名称 | 示例 | 用途 |
|---|---|---|
| 导入目录 | `/data/censorfs-demo/import` | 仅初始化时读取，形成第一个 Generation |
| 存储目录 | `/data/censorfs-demo/.censorfs` | 保存版本、对象、分支、Ticket、Journal 等 |
| 控制 Socket | `/data/censorfs-demo/control.sock` | CLI 与 daemon 之间的 Unix Socket |

正确顺序是：准备导入目录、初始化实例、启动 daemon、运行在线命令。

```bash
export PATH="$PWD/target/release:$PATH"
export CENSORFS_DEMO_ROOT=/data/censorfs-demo
export CENSORFS_STORAGE_ROOT="$CENSORFS_DEMO_ROOT/.censorfs"
export CENSORFS_SOCKET="$CENSORFS_DEMO_ROOT/control.sock"

mkdir -p "$CENSORFS_DEMO_ROOT/import"
printf 'hello from base\n' >"$CENSORFS_DEMO_ROOT/import/hello.txt"

censorfs-init --import-root "$CENSORFS_DEMO_ROOT/import" --branch main
censorfs-daemon >"$CENSORFS_DEMO_ROOT/daemon.log" 2>&1 &
CENSORFS_DAEMON_PID=$!

for _ in $(seq 1 100); do
  [[ -S "$CENSORFS_SOCKET" ]] && break
  sleep 0.05
done
test -S "$CENSORFS_SOCKET"

censorfs-info
censorfs-branch-list
censorfs-head main
```

`/data/censorfs-demo` 必须位于本地 XFS 或 ext4。可以用 `stat -f -c %T /data` 检查，输出应为 `xfs` 或 `ext2/ext3`。

测试结束后停止 daemon：

```bash
kill "$CENSORFS_DAEMON_PID"
wait "$CENSORFS_DAEMON_PID" 2>/dev/null || true
```

这套命令没有创建 FUSE mount，因此无需执行 `fusermount`。正常重启 daemon 也不要求手工删除失效 Socket；daemon 会检查并替换未被进程监听的旧 Socket 文件。

## 2. 为什么 `censorfs info` 会报 Socket 不存在

`censorfs-info` 是在线命令，它不会自动启动 daemon。下面的错误通常表示 CLI 找不到控制 Socket，不代表版本数据已经丢失：

```text
censorfs: IoError: No such file or directory (os error 2)
```

依次检查：

```bash
printf 'storage: %s\n' "$CENSORFS_STORAGE_ROOT"
printf 'socket:  %s\n' "$CENSORFS_SOCKET"
test -S "$CENSORFS_SOCKET" && echo 'socket exists' || echo 'socket missing'
pgrep -af 'censorfs-daemon|censorfs daemon|censorfsd'
tail -100 "$CENSORFS_DEMO_ROOT/daemon.log"
```

常见原因：

- 只执行了 `censorfs-init`，还没有启动 `censorfs-daemon`；
- daemon 因参数、锁或磁盘问题退出；
- CLI 和 daemon 使用了不同的 `CENSORFS_SOCKET`；
- 新终端没有重新设置环境变量。

推荐每个实例都显式设置 `CENSORFS_STORAGE_ROOT` 和 `CENSORFS_SOCKET`。如果不设置，存储目录默认为当前目录的 `.censorfs`，Socket 默认为存储目录父目录的 `control.sock`。

## 3. 全局参数

```bash
censorfs [GLOBAL_OPTIONS] <subcommand> [COMMAND_OPTIONS]
```

| 参数 | 含义 |
|---|---|
| `--storage-root DIR` | 覆盖 `CENSORFS_STORAGE_ROOT` |
| `--socket PATH` | 覆盖 `CENSORFS_SOCKET` |
| `--json` | 元数据命令输出 JSON |
| `--request-id UUID` | 为复合操作指定可重试请求 ID |

例如：

```bash
censorfs --storage-root /data/censorfs-demo/.censorfs \
  --socket /data/censorfs-demo/control.sock info --json
```

查看帮助：

```bash
censorfs --help
censorfs write --help
censorfs-merge --help
```

## 4. 命令总览

| 子命令 | 多调用名称 | daemon |
|---|---|---:|
| `censorfs init` | `censorfs-init` | 不需要 |
| `censorfs daemon` | `censorfs-daemon` | 启动 daemon |
| `censorfs info` | `censorfs-info` | 需要 |
| `censorfs branch-create` | `censorfs-branch-create` | 需要 |
| `censorfs branch-list` | `censorfs-branch-list` | 需要 |
| `censorfs head` | `censorfs-head` | 需要 |
| `censorfs explore` | `censorfs-explore` | 需要 |
| `censorfs ls`, `cat` | `censorfs-ls`, `censorfs-cat` | 需要 |
| `censorfs write`, `mkdir`, `rm`, `mv` | 对应 `censorfs-*` | 需要 |
| `censorfs commit`, `abort` | 对应 `censorfs-*` | 需要 |
| `censorfs diff`, `generation` | 对应 `censorfs-*` | 需要 |
| `censorfs merge` | `censorfs-merge` | 需要 |
| `censorfs fsck` | `censorfs-fsck` | 必须停止 daemon |

## 5. 初始化和服务状态

### `init`

```bash
censorfs-init --import-root DIR [--branch NAME]
```

它递归读取导入目录中的普通文件和目录，创建初始 Generation 和分支。存储目录必须尚未初始化；一个实例只执行一次。

```bash
censorfs-init --import-root /data/censorfs-base --branch main
```

### `daemon`

```bash
censorfs-daemon
```

daemon 独占存储实例、执行启动恢复并监听控制 Socket。同一个存储目录不能同时运行两个 daemon，也不能在 daemon 运行时执行 fsck。

### `info`

```bash
censorfs-info
censorfs-info --json
```

用于查看实例 ID、格式版本和恢复模式，要求 daemon 已经监听 Socket。

## 6. 分支和版本

列出分支：

```bash
censorfs-branch-list
censorfs-branch-list --json
```

从 `main` 的当前 Head 创建分支：

```bash
censorfs-branch-create experiment --from main
```

查看分支 Head：

```bash
censorfs-head experiment
censorfs-head experiment --json
generation=$(censorfs-head experiment --generation-only)
```

查看 Generation 类型、父节点和 Manifest 项数：

```bash
censorfs-generation "$generation"
censorfs-generation "$generation" --json
```

比较两个 Generation：

```bash
censorfs-diff LEFT_GENERATION RIGHT_GENERATION
censorfs-diff LEFT_GENERATION RIGHT_GENERATION --json
```

Diff 按路径排序，报告新增、删除、内容、元数据或类型变化。

## 7. 私有探索和文件命令

### 创建 Ticket

```bash
censorfs-explore BRANCH
ticket=$(censorfs-explore BRANCH --id-only)
```

高层 `explore` 会自动创建一个 Tx 和一个 Ticket，并固定为一个 Tx 对应一个 Ticket。Ticket 捕获分支当前 Head，后续写入在提交前只对该 Ticket 可见。

### 读取不同视图

`ls` 和 `cat` 必须且只能选择一个 selector：

```bash
censorfs-ls --branch main /
censorfs-ls --ticket "$ticket" /
censorfs-ls --generation "$generation" /
censorfs-ls --candidate "$candidate" /

censorfs-cat --branch main /hello.txt
censorfs-cat --ticket "$ticket" /hello.txt
censorfs-cat --generation "$generation" /hello.txt
```

逻辑路径必须以 `/` 开头。Branch、Generation 和 Candidate selector 只读；Ticket selector 可以看到未提交的 Upper。

### 修改 Ticket

整体写入或替换普通文件：

```bash
censorfs-write --ticket "$ticket" /note.txt --text 'hello'
censorfs-write --ticket "$ticket" /input.bin --from ./input.bin
producer | censorfs-write --ticket "$ticket" /stream.bin --stdin
```

`--text`、`--from` 和 `--stdin` 必须且只能指定一个。单次文件内容上限为 1 MiB；这项限制只属于演示 RPC，不代表 FUSE 数据面的文件大小上限。

目录和路径操作：

```bash
censorfs-mkdir --ticket "$ticket" /results
censorfs-mv --ticket "$ticket" /note.txt /results/note.txt
censorfs-rm --ticket "$ticket" /results/note.txt
censorfs-rm --ticket "$ticket" /results
```

删除目录时要求目录为空。修改命令只接受当前 Unix UID 拥有且状态为 `OPEN` 的 Ticket。

## 8. 提交和放弃

### `commit`

```bash
censorfs-commit TICKET [--message TEXT]
generation=$(censorfs-commit TICKET --message 'approved' --generation-only)
```

高层提交自动执行：

```text
Prepare -> Candidate -> Publish -> Close Tx
```

如果另一个提交已经推进同一 Branch Head，CAS 会返回 `HeadChanged`，不会覆盖新 Head。

完整例子：

```bash
censorfs-branch-create agent-a --from main
ticket=$(censorfs-explore agent-a --id-only)

censorfs-cat --branch agent-a /hello.txt
censorfs-write --ticket "$ticket" /hello.txt --text 'changed privately'
censorfs-cat --ticket "$ticket" /hello.txt

# 提交前稳定分支仍是旧内容
censorfs-cat --branch agent-a /hello.txt

generation=$(censorfs-commit "$ticket" \
  --message 'agent-a result' --generation-only)
censorfs-head agent-a
censorfs-generation "$generation"
censorfs-cat --branch agent-a /hello.txt
```

### `abort`

```bash
censorfs-abort TICKET
```

它会 Abort Ticket 和关联的高层 Tx。Branch Head 与稳定内容不变；该 Ticket 之后不能再 write 或 commit。

## 9. 三方 Merge

只检查是否可以合并，不落盘：

```bash
censorfs-merge source --into target --check
```

实际合并：

```bash
censorfs-merge source --into target --message 'merge source into target'
```

Merge 使用唯一最近共同祖先：

- Source 未变化：保留 Target；
- Target 未变化：采用 Source；
- 两侧结果相同：直接采用；
- 同一路径不同结果、目录删除与后代修改、文件/目录类型变化：报告冲突；
- 不做文本行级自动合并；
- 有冲突时不创建 Generation，不移动 Target Head；
- 成功 Generation 的父节点顺序固定为 `[target_head, source_head]`。

无冲突例子：

```bash
censorfs-branch-create source --from main
censorfs-branch-create target --from main

source_ticket=$(censorfs-explore source --id-only)
target_ticket=$(censorfs-explore target --id-only)

censorfs-write --ticket "$source_ticket" /source.txt --text 'source'
censorfs-write --ticket "$target_ticket" /target.txt --text 'target'
censorfs-commit "$source_ticket" --message 'source change'
censorfs-commit "$target_ticket" --message 'target change'

censorfs-merge source --into target --check
censorfs-merge source --into target --message 'merge demo'
censorfs-ls --branch target /
```

## 10. 幂等重试

`explore`、`commit`、`abort` 和 `merge` 是复合操作。客户端需要在超时或断线后安全重试时，应显式保存一个请求 ID：

```bash
request_id=$(cat /proc/sys/kernel/random/uuid)
censorfs --request-id "$request_id" \
  commit "$ticket" --message 'retryable commit'
```

结果未知时，使用同一个请求 ID 和完全相同的操作重试：

```bash
censorfs --request-id "$request_id" \
  commit "$ticket" --message 'retryable commit'
```

daemon 返回第一次操作对应的相同结果，不会再次移动 Head。不能把同一个请求 ID 复用于另一种操作或不同参数。

## 11. 离线 fsck

先停止 daemon：

```bash
kill "$CENSORFS_DAEMON_PID"
wait "$CENSORFS_DAEMON_PID" 2>/dev/null || true
```

只读检查：

```bash
censorfs-fsck
censorfs-fsck --json
```

显式执行安全修复：

```bash
censorfs-fsck --repair
```

`--repair` 可以截断 Journal 损坏残尾、选择有效 A/B 槽、补全可推导的 Receipt/关联/请求结果，并 Abort 没有 Candidate 的开放 Ticket。它不会修复损坏 Object、回拨 Head 或删除孤立 Object。

| 退出码 | 含义 |
|---:|---|
| 0 | 实例干净，或已完全安全修复 |
| 1 | 仍有未修复问题 |
| 2 | 参数、实例锁或 I/O 错误 |

fsck 完成后可使用相同的存储目录和 Socket 重新启动 daemon。

## 12. 常见错误

### `instance already locked`

同一存储目录已有 daemon 或 fsck 正在运行。停止现有进程后再启动第二个 daemon，运行 fsck 前必须停止 daemon。

### `backing filesystem ... is not XFS/ext4`

持久化目录不在受支持的本地文件系统上。把整个实例迁移到本地 XFS/ext4，不能通过关闭检查把不受支持的 backing store 用于生产。

### `only an OPEN ticket can create a writable view`

Ticket 已经 Prepare、Published、Aborted，或恢复时被安全 Abort。重新运行 `censorfs-explore BRANCH --id-only` 创建 Ticket。

### `HeadChanged`

Candidate 构建后，目标 Branch Head 已被其他提交推进。应从最新 Head 创建新探索，或把变更放到独立分支后显式 Merge。

### `merge conflicts`

Source 与 Target 对相同路径产生了不兼容结果。根据冲突列表，在新 Ticket 中手工整理内容后重新提交；CensorFS 不做文本自动合并。

### `file content exceeds the 1 MiB test RPC limit`

可读文件命令只用于核心逻辑展示。大文件和实际工作负载应通过 FUSE `/workspace` 数据面验证。

## 13. 场景脚本

先完成 release 构建，并让 `target/release` 位于 `PATH`：

```bash
export PATH="$PWD/target/release:$PATH"
```

然后在本地 XFS/ext4 数据盘上运行：

```bash
CENSORFS_DEMO_PARENT=/data bash scripts/demo-two-branch-isolation.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-merge.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-abort.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-crash-fsck.sh
CENSORFS_DEMO_PARENT=/data bash scripts/openeuler-parallel-worlds-smoke.sh
```

前四个脚本分别验证：双分支私有视图隔离与独立发布、无冲突/冲突 Merge、单分支 Abort、daemon `SIGKILL` 后的 fsck 检查与安全恢复。`openeuler-parallel-worlds-smoke.sh` 额外在 AArch64 真机上通过真实 FUSE 同时挂载三个同 Head Variant，运行 Bash/`rg`、只读 Candidate 验证、单冠军 Publish、stale 检查和失败世界 Abort。脚本结束时会打印保留的测试目录和 daemon 日志位置。

## 14. Harness Variant 机器接口

下面的命令始终输出一个稳定 JSON envelope：成功为 `{"ok":true,"result":...}`，失败为 `{"ok":false,"code":"HeadChanged","error":"..."}` 并返回非零退出码。调用方必须持久化顶层 request ID；复合步骤在 daemon 内使用它派生确定性的子请求 ID。

```bash
head_json=$(censorfs --json head main)
generation=$(jq -r .generation_id <<<"$head_json")
head_seq=$(jq -r .head_seq <<<"$head_json")
run_id=demo-run
variant_id=minimal

censorfs --request-id "$(cat /proc/sys/kernel/random/uuid)" variant-open \
  --branch main \
  --expected-generation "$generation" \
  --expected-head-seq "$head_seq" \
  --run "$run_id" --variant "$variant_id"
```

`variant-open` 返回 Tx、Ticket、可写 View、owner UID/GID 和最初 Head。完整 worker 应由 `censorfs-mounter` 使用该 View 启动，不能只替换 Harness 的 `ctx.fs`。

worker 退出后关闭并冻结世界：

```bash
censorfs --request-id "$prepare_request_id" variant-prepare \
  --ticket "$ticket" --view "$view" \
  --run "$run_id" --variant "$variant_id" \
  --timeout-ms 30000 --max-diff-file-bytes 262144
```

结果包含 Candidate、Generation、路径 Diff 和文本统一 Diff。二进制、单文件超限、响应总量超限、非文件和类型变化只返回类型、大小及 digest 摘要，不嵌入内容。

Candidate 验证或预览使用一次性只读 View：

```bash
censorfs candidate-view-open --candidate "$candidate"
censorfs view-close --view "$candidate_view"
```

用户采用冠军时按最初 Head CAS 发布；若 Head 已变化，返回 `HeadChanged`，Candidate 保留：

```bash
censorfs --request-id "$publish_request_id" variant-publish \
  --candidate "$candidate" \
  --expected-generation "$generation" --expected-head-seq "$head_seq" \
  --decision-id "session-42:user-choice" \
  --run "$run_id" --variant "$variant_id"
```

失败、取消、超时及未采用方案使用同一个 request ID 重试 `variant-abort`，它会容忍 View 已关闭，并依次 Abort Ticket/Candidate/Tx：

```bash
censorfs --request-id "$abort_request_id" variant-abort \
  --ticket "$ticket" --view "$view" \
  --run "$run_id" --variant "$variant_id"
```

完整 DeepSeek Harness bundle、安装方法、竞技场 UI 和三个演示工程见 [`integrations/deepseek-harness/README.md`](../../../../CensorFs/integrations/deepseek-harness/README.md)。
