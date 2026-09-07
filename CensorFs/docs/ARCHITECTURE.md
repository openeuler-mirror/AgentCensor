# CensorFS v0.1 架构说明

## 1. 设计目标

CensorFS 为多个 Agent 提供从同一稳定版本出发的隔离工作区。核心目标是：

- 未提交修改只在对应 Ticket View 中可见；
- 每个已发布版本不可变、可定位、可比较；
- 同一分支的并发发布不会静默覆盖；
- 回滚保留完整历史，不直接把 Head 拨回旧值；
- daemon 崩溃后可以根据磁盘记录恢复到可解释状态；
- Agent 只能看到 `/workspace`，不能直接访问持久化内部目录。

v0.1 不提供跨分支原子提交、自动文本冲突解决、动态策略、Merkle Manifest、内容去重或垃圾回收。

## 2. 进程和代码分层

```text
Agent process
    |
    | /workspace in a private Mount Namespace
    v
censorfs-mounter ---- passes /dev/fuse FD ----> censorfsd
                                               |
censorfsctl / censorfs ---- Unix Socket RPC ----+
                                               |
                                               v
                                      censorfs-core
                          ViewEngine / Branch / Store / Persist
```

### 进程职责

- `censorfsd` 是唯一长期持有实例锁的进程，负责启动恢复、状态机、控制接口和所有 FUSE Session。
- `censorfsctl` 是底层无特权控制客户端，不参与 Agent 的普通文件读写。
- `censorfs-mounter` 是短生命周期特权助手：创建 Mount Namespace、挂载固定路径 `/workspace`、向 daemon 移交 FUSE FD、降权并 `exec` Agent。
- `censorfs` 是可读的多调用命令，复用 daemon 的控制接口和核心模型，适合演示与测试。

### 核心模块

| 层 | 主要模块 | 职责 |
|---|---|---|
| 模型与协议 | `ids`, `model`, `codec`, `api_generated` | 强类型 ID、状态对象、稳定编码和 Protobuf 类型 |
| 持久化 | `persist`, `store`, `fault` | 原子记录、A/B 槽、Journal、Object/Manifest/Generation 和故障注入 |
| 版本状态机 | `branch`, `fsck` | Tx/Ticket、Prepare、Publish、回滚、Merge、恢复和离线检查 |
| 文件视图 | `upper`, `viewfs` | Ticket Upper/Delta、copy-up、路径操作、句柄和目录快照 |
| Linux 接入 | `control`, `fuse_adapter`, `namespace` | Unix Socket、凭据、FUSE、Mount Namespace 和降权 |

`ViewEngine` 与 FUSE 适配层分离：核心文件语义可以不创建 mount 直接测试，`fuse_adapter` 只把同一套操作映射到 Linux FUSE 请求。

## 3. 核心对象

```text
Branch Head --CAS--> Generation --> Manifest --> Object
                        ^              |
                        |              +--> path, type, mode, times, object ID
Ticket Upper + Delta --Prepare--> Candidate

Historical Generation --rollback contents--> new Rollback Generation
Source + Target + merge base --three-way merge--> new Merge Generation
```

- `Object` 保存普通文件内容，使用随机 UUID 标识，并另外保存 BLAKE3 摘要。
- `Manifest` 是逻辑路径到文件/目录元数据的完整有序映射。
- `Generation` 引用一个 Manifest 和一个或两个父 Generation；正式落盘后不可修改。
- `Branch Head` 是可变发布点，包含 `generation_id` 和单调递增的 `head_seq`。
- `Tx` 聚合控制操作；一个 Tx 可以关联多个 Ticket，但 v0.1 不保证跨分支原子性。
- `Ticket` 捕获某分支的基准 Head，并拥有私有 Upper 和 Delta。
- `Candidate` 是已 Prepare、尚待发布到单一 Branch 的 Generation。

`head_seq` 与 Generation ID 一起参与 CAS，可以防止 Head 在不同历史操作中出现 ABA。

## 4. 状态机

Ticket 状态：

```text
OPEN -> FREEZING -> PREPARED -> PUBLISHED
  \         \          \
   +---------+-----------+--> ABORTED
```

- `OPEN` 接受 Ticket View 写入。
- `FREEZING` 禁止新 mutation，并等待正在执行的 mutation 和可写 FD 结束。
- `PREPARED` 已形成 Candidate，不再接受写入。
- `PUBLISHED` 对应 Candidate 已通过 CAS 发布。
- `ABORTED` 的私有 Upper 不再可达。

View 状态：

```text
CREATED -> MOUNTED -> REVOKED -> CLOSED
```

View ID 只在当前 daemon mount epoch 内有效；Tx、Ticket、Candidate 和 Generation ID 是持久 ID。

## 5. 写入、Prepare 和 Publish

1. `BEGIN_TX` 由文件系统持久分配 Tx ID。
2. `BEGIN_TICKET` 读取并记录分支当前 `(generation_id, head_seq)`，持久分配 Ticket ID。
3. `OPEN_VIEW(Ticket)` 创建当前 daemon epoch 的临时 View ID。
4. Agent 写操作只修改 Ticket 的 Upper 与 Delta；第一次写普通文件时执行 copy-up。
5. `PREPARE_TICKET` 将状态切到 `FREEZING`，停止新写入并等待活动写操作结束。
6. 按 `Object -> Manifest -> Generation -> Candidate -> Ticket` 顺序固化记录。
7. `PUBLISH` 在分支锁内再次校验 Head，写入 `HEAD_SWITCH_PREPARE`，更新非活动 Head 槽并 fsync，再写 `HEAD_SWITCH_COMMIT` 和 Receipt。

如果实际 Head 与 Candidate 的基准 Head 不一致，发布返回 `HeadChanged`。Candidate 保留用于检查，不会自动覆盖或自动 Merge。

高层 `censorfs commit` 把 Prepare、Publish 和关闭 Tx 组合为一个可幂等续跑的操作。外部只提供一个 `request_id`，内部步骤使用确定性派生的子请求 ID。

## 6. 回滚

未发布 Ticket 的回滚就是 Abort：丢弃其私有 Upper，Branch Head 不变。

已发布内容的回滚不会直接修改 Head 指向旧 Generation。`BUILD_ROLLBACK_CANDIDATE`：

1. 读取目标历史 Generation 的 Manifest；
2. 复用其中已有 Object 引用；
3. 创建内容等于历史目标、父节点等于当前 Head 的新 Rollback Generation；
4. 通过正常 Candidate Publish 推进 Head。

因此审计历史始终单调向前，可以区分“原历史版本”和“执行回滚后产生的新版本”。

## 7. 三方 Merge

显式 Merge 首先寻找 Source 与 Target 的唯一最近共同祖先。存在多个同优先级 merge base 时拒绝继续。

逐路径规则：

| Source 相对 Base | Target 相对 Base | 结果 |
|---|---|---|
| 未变化 | 任意 | 保留 Target |
| 任意 | 未变化 | 采用 Source |
| 两侧最终结果相同 | 两侧最终结果相同 | 直接采用 |
| 两侧结果不同 | 两侧结果不同 | 冲突 |

目录删除与另一侧后代修改、文件/目录类型变化也属于结构冲突。v0.1 不做文本行级合并。

有冲突时，按路径排序返回冲突列表，不创建 Candidate/Generation，也不移动 Target Head。成功时复用现有 Object，创建父节点顺序固定为 `[target_head, source_head]` 的 `GenerationKind::Merge`。

Merge 发布阶段按 Branch ID 顺序取得 Source/Target 锁，再次检查两个 Head。Source 发生变化或 Target CAS 失败均返回 `HeadChanged`。

## 8. View 和 POSIX 语义

- 每个 View 的 inode 由 `View ID + logical path` 派生，不在 View 间共享。
- Ticket View 使用 `direct_io`、零属性/目录 TTL，并禁用 writeback cache 和共享可写 mmap。
- Branch、Candidate 和历史 Generation View 只读，可以使用只读页缓存。
- 打开文件句柄保存文件身份；rename 后句柄跟随新路径，unlink 后旧句柄仍可读且不会复活目录项。
- 目录句柄在 `opendir` 时冻结 readdir 快照。
- 路径 mutation 使用同时覆盖祖先/后代冲突的有序锁，避免相反 rename 等并发死锁。
- Upper 通过预打开目录 FD 与 `openat2` 访问，并要求 `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV | RESOLVE_NO_SYMLINKS`。

## 9. Namespace 和权限边界

1. mounter 执行 `unshare(CLONE_NEWNS)`。
2. 将 `/` 递归设为 `MS_PRIVATE`，禁止 mount 事件向宿主或其他 Agent 传播。
3. 只允许固定挂载目标 `/workspace`。
4. 打开 `/dev/fuse`，完成 mount，并把 FD 连同 View ID 发送给 daemon。
5. daemon 验证调用者、View owner 和只读属性后启动 FUSE Session。
6. mounter 清空补充组、能力和提权通道，切换到 Agent UID/GID 后执行 Agent。

daemon 独占 `.censorfs`；Agent 和普通 ctl 用户不应获得该目录的权限。内部根目录及子目录固定为 `0700`，不依赖进程 umask。

控制接口不信任请求中的 actor 字段，身份只取 Unix Socket `SO_PEERCRED`。非 root 调用者只能操作自己的 Tx、Ticket、Candidate 和私有 View；FUSE FD attach 只接受 UID 0 的可信 mounter。

## 10. 恢复与 fsck

daemon 启动恢复遵守以下不变量：

- Journal 解析到不完整 frame 或 CRC32C 错误残尾时，截断到最后一个有效 frame。
- A/B 槽选择校验有效且序号最大的记录；同序号指向不同值视为损坏。
- Candidate 已正式落盘但 Ticket 仍为 `OPEN/FREEZING` 时，恢复关联并设为 `PREPARED`。
- 没有 Candidate 的 `OPEN/FREEZING` Ticket 进入 `ABORTED`，v0.1 不恢复其未 Prepare 写层。
- Head 已切换但 Receipt 缺失时，根据 Head、Candidate 和 Journal 补全 Receipt 与发布状态。
- 当前 Head 引用的 Generation、Manifest 或 Object 损坏时，实例进入只读模式。

`censorfs-fsck` 使用与 daemon 相同的实例锁，只能离线运行。默认检查不创建目录、不截断日志、不改状态；`--repair` 只执行可以从现有正式记录唯一推导的恢复动作，并在修复后自动复检。

fsck 不修复损坏 Object、不回拨 Head、不猜测冲突状态，也不删除孤立 Object。无法安全修复的当前 Head 引用损坏会作为只读原因保留并返回非零状态。

## 11. 持久化和架构兼容性

所有磁盘整数使用固定宽度小端编码，不写入 Rust 内存布局、指针或 `usize`。AArch64 和 x86-64 构建可以读取同一实例；可执行文件仍必须按 CPU 架构分别编译。

生产 backing store 只支持本地 XFS/ext4。该约束保证临时文件与目标记录位于同一文件系统，原子 rename、文件 fsync 和父目录 fsync 具有设计依赖的语义。详细记录格式见 [磁盘格式](ON_DISK_FORMAT.md)。
