# 贡献与提交

CensorPrivate 是 AgentCensor 的模型数据隐私子项目。按可评审的功能层组织提交，避免把完整功能一次性压成一个超大 commit。当前导入采用每个提交新增与删除合计不超过 1000 行的要求；此限制同样统计测试、文档及锁文件，不靠忽略生成文件放宽。

## 开发与检查

使用 Rust 1.85+，保持核心 crate 禁止 unsafe；必要系统调用放到 `crates/runtime-sys`。新增能力必须同时说明授权边界、失败行为和验证方法，不允许原文回退。修改前阅读 [架构说明](docs/architecture.md)。

```bash
bash scripts/test/run-all.sh
bash scripts/test/run-all.sh --native --runtime --operations
```

扩展运行域测试需要 root 和实际 namespace/mount/nft 能力；DSH 与真实云验收按 [使用手册](docs/usage.md) 单独执行。纯文档变更检查链接和示例即可，不必重复运行所有集成测试。

## Commit

使用 `<type>(<scope>): <subject>`，主题简洁，正文解释问题、行为和验证，按项目要求附 `Signed-off-by`。使用仓库已配置的真实作者身份，不跳过现有 Git hooks。

```text
feat(policy): validate immutable privacy policies

- reject unknown fields, invalid entity types and unbounded TTL
- preserve UTF-8 ranges and fail when required detection fails
- verify policy and detection regression tests

Signed-off-by: Your Name <your-address@example.org>
```

暂存后运行 `bash scripts/check-commit-size.sh --staged`。检查整个提交序列可用 `bash scripts/check-commit-size.sh BASE..HEAD`；初始历史可用 `--all`。统计是新增加删除，不按净行数计算，也不通过关闭重命名统计隐藏大规模修改。

不要按固定第 1000 行截断一个函数、JSON 或锁文件。先拆职责，再按依赖层引入模块及依赖。Cargo.lock 必须是 Cargo 可验证的完整文件，依赖按功能分组加入；底层实现的连接点在该批次启用后再暴露，不能以 TODO 或空实现代替功能。

## PR 批次

1. 初始化项目骨架与严格解析，提交可构建的最小库、许可证和目录说明。
2. 策略与检测，提交词典、规则、本机检测器、UTF-8 与固定样例测试。
3. 引用与计算，提交 run vault、期限/标签、精确运算与授权测试。
4. 协议与 gate，提交被动显示、一次性放行、SSE 及边界测试。
5. 网关与 CLI，逐组引入完整依赖锁文件，实现认证 HTTP/HTTPS 转发。
6. 工具与 HTTPS 验收，提交 stdio MCP、私密计算客户端和真实进程测试。
7. Linux 运行域，提交 syscall 边界、隔离计划、执行监督及实际隔离测试。
8. TLS direct，提交原字节映射、broker、native hook、launcher 与真实 OpenSSL 测试。
9. 运维与 DSH，提交部署入口、插件、可复现模拟演示及失败路径测试。
10. 文档与交付，提交构建、部署、策略/凭据准备、使用手册、验收与 CI。

每个 PR 依赖前一个已合入批次，或以明确 base 建立堆叠 PR；不要把仍在审查的前置代码重复拷贝进新提交。每批包含若干小 commit，每个 commit 遵守行数上限；PR 总规模如另有社区限制，再继续按接口与测试边界拆分。实际导入顺序见 [提交序列说明](docs/submission-plan.md)。

PR 正文先描述新增能力及具体前后行为，再给出边界、测试命令和依赖批次。区别本机模拟、真实 DSH agent、系统服务验证与真实云请求，不能以 readiness 或模拟回复宣称真实云效果通过。不提交 token、key、运行报告、日志、构建产物或个人环境文件。
