# Censord 最小生命周期管理计划

## 目标

提供一个不包含批次事务流程的统一 `censord`，负责初始化、启动、检查并完整停止
CensorFS、CensorGuard 和 CensorScope。

## 实施项

- [x] 核对三个组件的真实 init、前台运行和 doctor 命令。
- [x] 新增 `censord init/run/doctor` 和独立配置模型。
- [x] 实现启动就绪检查、子进程异常退出联动和 TERM/KILL 收尾。
- [x] 增加示例配置、使用文档和生命周期测试。
- [x] 执行 `cargo fmt/check/test/clippy`。

## 验收标准

1. `init` 可重复执行，不重复初始化已有 CensorFS，并校验 Guard/Scope 配置。
2. `run` 直接监督三个前台 daemon；任一组件退出或收到终止信号时停止全部组件。
3. 每个组件运行在独立进程组并设置父进程死亡信号，正常退出先 TERM，超时后 KILL。
4. `doctor` 对三个真实控制接口执行往返检查，并以机器可读 JSON 报告结果。
5. 不调用 `VariantOpen/Prepare/Publish/Abort`，不执行 Agent 工具。
