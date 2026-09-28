# CensorPivot 初版实施计划

## 目标定义

在仓库根目录新增独立 Rust 组件 `CensorPivot`，作为 Agent 工具调用的统一接入与编排层：调用方只向 CensorPivot 提交批次，CensorPivot 负责把整批调用放进同一 CensorFS 私有视图、交给 CensorGuard 强制约束、关联到 CensorScope，并以持久化两阶段状态机完成提交或放弃。

## 功能分解

- 统一接入：Unix Socket 上的 JSON Lines 协议和配套 CLI，限制帧大小、校验调用参数、支持幂等请求 ID。
- 批次事务：顺序执行一批工具调用；全部成功才允许 Commit，任一失败或策略拒绝即 Abort。
- 两阶段推进：执行/冻结属于 Prepare；决策日志 durable 后进入 Commit/Abort；决策一旦写入禁止反转。
- 崩溃恢复：日志采用原子临时文件 + rename + 目录 fsync；启动扫描未终态事务并重放已决定的第二阶段。
- 组件适配：调用现有 `censorfs`/`censorfs-mounter`、`censorguard-exec`、`censorscopectl`，不侵入三个已有组件。
- 可测试性：核心状态机依赖 trait，使用内存/脚本化 fake 覆盖成功、准备失败、决策不可变、恢复重放与幂等。
- 运维入口：`serve`、`submit`、`status`、`recover`、`doctor`，配置文件给出生产命令和 socket 默认值。

## 实施步骤

- [x] 读取三个组件已有控制面、执行边界与版本约束。
- [x] 建立 CensorPivot crate、配置模型、协议与领域状态机。
- [x] 实现 durable transaction store 和恢复逻辑。
- [x] 实现 CensorFS、CensorGuard、CensorScope 命令适配器与工具执行器。
- [x] 实现 UDS daemon、CLI 和示例配置。
- [x] 编写设计文档，说明一致性、不变量、失败矩阵和安全边界。
- [x] 增加无需 Mermaid 的七层文本框架图与 I0-I6 接口矩阵，标明 socket、协议、组件接口、执行链和数据流。
- [x] 运行 fmt、clippy、test，并修正问题。

## 验收标准

- 仓库根目录存在 `CensorPivot/`，可独立 `cargo build`。
- 能通过单个批次请求执行多个工具调用，并返回逐调用结果和最终事务状态。
- Commit/Abort 决策持久化后不能被后续请求翻转；进程重启能继续第二阶段。
- Guard 不可用时默认拒绝执行；Scope 可配置为必需或尽力而为。
- CensorFS 发布使用原始 expected head CAS，冲突不会被静默覆盖。
- 单元测试覆盖核心协议不变量，设计文档足以指导后续生产化。
