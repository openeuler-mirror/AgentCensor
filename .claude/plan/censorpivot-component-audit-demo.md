# CensorPivot 组件接口审计与 Demo 实施计划

## 目标

以仓库当前代码为唯一事实来源，明确 CensorFS、CensorGuard、CensorScope
已实现的功能与可用接口，划清 CensorPivot 应实现的编排边界，并提供一个
“生成产物 + 校验产物”的可运行原子批次 Demo。

## 实施项

- [x] 从协议定义、CLI 参数和服务端分发代码交叉核对三个组件。
- [x] 新增三组件功能与实现接口说明，标明已实现、尚未实现和 Pivot 责任。
- [x] 增加 runner 就绪握手，使 Pivot 拿到真实受控进程 PID。
- [x] 使 Pivot 通过 CensorScope `track-add/track-remove` 自动管理批次观测生命周期。
- [x] 修正 CensorScope call status 映射，仅发送组件接口接受的枚举值。
- [x] 新增单场景 Demo 请求模板、Rust 执行程序、包装脚本与使用说明。
- [x] 运行 Rust 格式、Clippy、单元测试、Demo 脚本语法和 Markdown 检查。

## 验收标准

- 功能说明中的每个“已有接口”都能定位到仓库内的协议或实现代码。
- 文档明确回答“无接口时是否在 Pivot 实现”，不把数据面能力复制到 Pivot。
- Pivot 在工具运行前可得到真实 runner PID，Scope 能关联整个批次及每次调用。
- Demo 仅展示一个场景：两个工具调用全部成功才 Commit，任一失败即 Abort。
- 无特权环境下可完成静态和单元验证；真实 FUSE/eBPF 联调前置条件在文档中明确列出。
