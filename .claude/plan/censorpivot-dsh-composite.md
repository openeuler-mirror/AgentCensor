# CensorPivot DSH 组合插件计划

## 目标

提供一个 CensorPivot 安装入口，把 CensorFS、CensorGuard、CensorScope 已有 DSH
插件按 1+1+1 方式组合使用，不融合或重写各模块实现。

## 实施项

- [x] 核对三个模块插件清单、profile 归属和 DSH bundle 激活机制。
- [x] 新增 CensorPivot Web 聚合 bundle 与内部 Headless 配套 bundle。
- [x] 新增统一打包脚本，将原插件转换为不重复激活 patch 的运行时包。
- [x] 新增一次安装入口，同时配置 web/headless profile。
- [x] 增加组合结构、打包产物和 shell 语法测试。
- [x] 更新文档并运行相关 Node、shell、Rust 校验。
- [x] 适配 DSH 0.1.5-rc.2 的 `uiConversation` 客户端服务并增加回归断言。
- [x] 适配 DSH 0.1.5-rc.2 的句柄式 SessionPersistence（新建与 resume）。

## 验收标准

1. 用户只执行一个 CensorPivot 安装脚本。
2. Web profile 只激活 `@agentcensor/censorpivot` 聚合层。
3. Headless profile 只额外激活内部配套层，用于 FS event-exporter 和 Scope worker。
4. FS、Guard、Scope 原插件源代码不被复制修改，打包时作为运行时包复用。
5. 聚合 patch 保留三个模块现有能力和配置，不实现跨模块融合逻辑。
