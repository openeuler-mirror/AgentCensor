# CensorFS v0.1 磁盘格式（format version 1）

本文描述 `.censorfs` 的稳定记录格式、目录布局和恢复约束。实现只允许 `censorfsd` 或离线 `censorfs-fsck` 持有实例锁；外部程序不应直接修改这里的文件。

## 1. 兼容性原则

- 格式版本为 `1`，每条正式记录都携带版本和记录类型。
- 整数使用固定宽度、小端编码；payload 使用 bincode fixed-int little-endian 配置。
- 不写入 Rust 结构体内存布局、指针、`usize` 或 CPU 原生 endian。
- AArch64 与 x86-64 可以读取同一实例，但各自需要本架构可执行文件。
- 已分配的记录类型和枚举值不能重排或复用；新增类型只能追加。
- 遇到未知格式版本或记录类型时拒绝写入，不能猜测解析。

## 2. 正式记录编码

每个元数据记录都有 53 字节 header（CensorFS 品牌改名后 magic 加宽，格式版本升至 2；旧 `YUSHIFS`/v1 store 不再可读）：

| 字段 | 大小 | 编码 |
|---|---:|---|
| magic/type | 9 B | `CENSORFS` 加记录类型字节 |
| format version | 2 B | `u16` little-endian；当前磁盘格式为 2 |
| record kind | 2 B | `u16` little-endian |
| payload length | 8 B | `u64` little-endian |
| payload digest | 32 B | BLAKE3(payload) |
| payload | 可变 | bincode fixed-int little-endian |

解码时必须同时验证 magic、格式版本、类型、精确长度、BLAKE3 摘要和 payload 尾部。任何不一致都视为损坏，不进行部分反序列化。

format version 1 的记录类型：

| 值 | 类型 | 典型内容 |
|---:|---|---|
| 1 | Superblock | 实例 ID、格式和全局序号 |
| 2 | BranchMeta | Branch ID 和创建基线 |
| 3 | HeadSlot | Generation ID 与 `head_seq` |
| 4 | Generation | 父节点、Manifest、类型和来源 |
| 5 | Manifest | 完整有序路径映射 |
| 6 | Object | 普通文件内容和摘要 |
| 7 | Tx | Tx 状态与 Ticket 列表 |
| 8 | Ticket | 基准 Head、状态和 Candidate 关联 |
| 9 | Candidate | 待发布 Generation 和目标 Branch |
| 10 | Receipt | 已完成 Head 切换的发布回执 |
| 11 | RequestResult | `request_id -> operation/result` |
| 12 | MergeIntent | Source/Target Head 和 merge base |

## 3. Journal frame

Journal 采用追加 frame：

```text
u32 payload_length (little-endian)
payload (bincode fixed-int little-endian)
u32 CRC32C(payload) (little-endian)
```

payload 中的 `JournalRecord` 包含单调序号、操作 Request ID、记录类别、目标 ID 和关联 payload 摘要。

format version 1 的 Journal 类别包括：

- `BranchCreated`
- `CandidateReady`
- `RollbackCandidateReady`
- `HeadSwitchPrepare`
- `HeadSwitchCommit`
- `TicketAborted`
- `MergeCandidateReady`

读取遇到不完整长度、payload、CRC 或不可解析 frame 时停止。恢复/修复只能把文件截断到最后一个完整有效 frame，不能跳过损坏字节继续扫描。

## 4. 目录布局

```text
.censorfs/
├── lock
├── superblock/
│   ├── slot.a
│   └── slot.b
├── branches/<hex-branch-id>/
│   ├── branch.meta
│   ├── head.a
│   └── head.b
├── generations/<generation-uuid>.meta
├── manifests/<manifest-uuid>.manifest
├── objects/<object-uuid>.data
├── txs/<tx-uuid>.meta
├── tickets/<ticket-uuid>/
│   ├── ticket.meta
│   ├── delta.log
│   └── upper/...
├── candidates/
│   ├── <candidate-uuid>.meta
│   └── <candidate-uuid>.merge
├── publish/<hex-branch-id>/<head-seq>.receipt
├── requests/<request-uuid>.result
├── journal/current.log
├── tmp/
└── trash/
```

实例根目录及内部目录权限固定为 `0700`。控制 Socket 位于实例外部，不属于磁盘格式。

## 5. Object、Manifest 和 Generation

### Object

Object ID 是随机 UUID，不等于内容摘要。Object 记录另外保存 BLAKE3 内容摘要，加载时复验。v0.1 不按内容自动去重；Merge 和回滚可以复用已经存在的 Object 引用。

### Manifest

Manifest 是 `BTreeMap<LogicalPath, ManifestEntry>`，按规范化逻辑路径完整排序。条目保存：

- 文件或目录类型；
- mode；
- size；
- atime/mtime 纳秒值；
- 普通文件的 Object ID 和内容摘要。

Manifest 必须满足根目录、父目录存在、类型一致、路径规范化和条目数量上限等校验。v0.1 使用完整 Manifest，不使用 Merkle 分块。

### Generation

Generation 保存 Manifest ID/摘要、父节点、类型、来源 Ticket/Candidate 和条目数：

- Initial Generation 没有父节点；
- Normal/Rollback Generation 的父节点是发布时的目标 Branch Head；
- Merge Generation 的父节点固定为 `[target_head, source_head]`。

Generation、Manifest 和 Object 是不可变记录；创建后不覆盖。

## 6. 原子写入规则

正式记录统一执行：

1. 在同一实例的 `tmp/` 使用随机名称创建新文件；
2. 写入完整 header 和 payload；
3. 对临时文件执行 `fsync`；
4. 在同一 backing store 内原子 rename 到目标路径；
5. 对目标父目录执行 `fsync`。

Linux 创建型记录使用 `renameat2(RENAME_NOREPLACE)`，防止并发创建静默覆盖已有记录。上述协议依赖同一块本地 XFS/ext4 的 rename 和 fsync 语义，因此 `.censorfs` 不能跨文件系统，也不支持网络、Overlay 或 FUSE backing store。

## 7. A/B 槽

Superblock 和 Branch Head 使用 A/B 双槽。更新时：

1. 读取两个槽并选出校验有效、序号最大的活动值；
2. 把新值写入非活动槽；
3. 完成文件 fsync、rename 和目录 fsync；
4. 下次更新再写另一个槽。

Branch `head_seq` 每次成功发布严格加一。一个槽损坏时可选择另一个有效槽；两个有效槽具有相同序号但不同内容时视为不可安全判定的损坏。

## 8. Publish 持久化顺序

普通 Candidate 的 Prepare 顺序为：

```text
Object -> Manifest -> Generation -> Candidate -> Ticket(PREPARED)
```

Publish 顺序为：

```text
HEAD_SWITCH_PREPARE journal
    -> inactive Head slot
    -> HEAD_SWITCH_COMMIT journal
    -> Publish Receipt
    -> Candidate/Ticket terminal state
```

恢复不依赖“最后一条状态文件一定写完”。如果 Head 已切换但 Receipt 或终态缺失，可使用 Candidate、Head 和 Journal 唯一推导并补全；如果 Head 未切换，则不能凭 `PREPARE` 记录假定发布成功。

## 9. 请求幂等记录

创建型和高层复合操作把 `request_id`、操作名和序列化结果持久化到 `requests/<uuid>.result`。同一 Request ID：

- 重试相同操作时返回原结果；
- 用于不同操作或不兼容参数时必须拒绝；
- 复合操作的内部步骤使用从外部 Request ID 确定性派生的子 ID。

如果最终 RequestResult 缺失，但正式状态已经足以唯一推导结果，恢复或 `fsck --repair` 可以重建；不能唯一推导时不得猜测。

## 10. fsck 边界

离线 fsck 扫描 A/B Superblock、Branch Head、Journal、Tx、Ticket、Candidate、Receipt、Generation、Manifest、Object 和请求结果。

安全修复范围：

- 截断 Journal 损坏残尾；
- 选择最新有效 A/B 槽；
- 补全 Head 已切换但缺失的 Receipt；
- 恢复 Candidate/Ticket 关联；
- Abort 没有 Candidate 的开放 Ticket；
- 重建可以唯一推导的请求结果。

明确不做：

- 生成或替换损坏 Object；
- 把 Head 回拨到历史 Generation；
- 猜测同序号冲突槽的正确值；
- 自动删除孤立 Object、Manifest 或 Generation。

无法修复的当前 Head 引用损坏会使实例保持只读，并让 fsck 返回非零状态。

## 11. 格式演进规则

升级实现时必须：

1. 保留所有既有 RecordKind、状态和 Journal 枚举值；
2. 新字段采用显式格式版本或可验证的迁移过程，不能依赖 Serde 默认值隐式改变旧记录；
3. 在修改写入逻辑前提供旧格式读取、升级中断恢复和跨架构读取测试；
4. 未完成迁移前保留旧记录，禁止原地不可逆覆盖；
5. 对不支持的更高格式版本以只读或拒绝打开方式失败。

v0.1 不实现在线格式升级；部署新格式前应先备份实例并完成离线迁移验证。
