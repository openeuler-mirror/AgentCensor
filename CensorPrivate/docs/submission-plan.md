# 初始功能导入与 PR 顺序

CensorPrivate 作为 AgentCensor 的隐私子项目独立构建。初始导入只提交 Rust 产品、必要的 OpenSSL C ABI 接入、测试和交付文件；不包含历史语言实现、迁移资料、凭据或本机产物。旧开发历史保留在原分支，初始导入分支为 `feat_rust_initial`，两者没有共同祖先。

该分支用于向新建的空仓库或 AgentCensor 约定的子项目位置导入。向已有历史的目标仓库提 PR 时，应从目标分支创建导入分支，将下列提交按顺序 cherry-pick 到约定目录；不要强制推送替换远端历史，也不要直接向旧 Python 主分支提交无共同祖先的 PR。仓库当前没有修改 AgentCensor 主仓库的注册或构建配置。

## 评审批次

每个 commit 的新增与删除合计最多 1000 行，测试、文档、锁文件全部计入。PR 可以包含多个小 commit；如目标社区还限制 PR 总行数，按表中接口与验证边界继续拆 PR。

| 批次 | 提交序号 | 新增能力 | 本批验证 |
| --- | --- | --- | --- |
| PR1 | 01–02 | 许可证、最小 Rust 库、稳定错误与严格 JSON | 最小构建、重复键和深度限制单元测试 |
| PR2 | 03–04 | 策略校验、随机引用语法、本机检测 | 策略/检测测试及 80 组固定样例 |
| PR3 | 05–07 | run vault、授权、精确计算与派生引用 | 并发、期限/epoch、秘密拒绝及 127 组计算样例 |
| PR4 | 08–12 | 协议字段分类、被动显示、gate、SSE | gate 与流式边界、分片及工具参数测试 |
| PR5 | 13–18 | 异步/HTTP/TLS 依赖、认证网关与 CLI | 实际回环上游、压缩、认证、错误路径测试 |
| PR6 | 19–20 | 本机工具客户端、stdio MCP、HTTPS 验收 | MCP schema、实际 HTTPS 进程、重载与 privcalc |
| PR7 | 21–24 | Linux syscall 边界、计划、bootstrap、监督与 launcher | 运行域单元测试、真实降权/挂载/网络及连续超时清理 |
| PR8 | 25–28 | 原字节映射、broker、OpenSSL hook 与 TLS launcher | 等长替换、转义、分片、重试及真实 ABI 测试 |
| PR9 | 29–31 | cpctl、环境/部署、DSH 插件与本机演示 | 插件拒绝路径、安装后生命周期、网关/TLS 演示 |
| PR10 | 32–35 | 构建/使用/验收说明、提交约定与 CI | 全套检查、发布归档与逐提交行数检查 |

依赖按功能引入，Cargo 生成每层完整锁文件。网关批次先实现常规模型路径；TLS 批次再连接 broker 路由和别名转换。运行域先引入计划及 bootstrap，再连接执行监督，不使用空实现占位。暂未被上层连接的内部帮助函数可能产生 dead-code 提示；最终项目执行 Clippy 并将警告视为错误。

## 验证提交序列

每个提交实际执行 `cargo check --offline --all-targets` 后才保存。离线检查需要预先下载依赖；新环境可先运行 `cargo fetch --locked`。各 PR 到达表中的完整功能边界时执行相应测试，最终执行：

```bash
bash scripts/check-commit-size.sh --all
bash scripts/test/run-all.sh --native --runtime --operations
bash scripts/test/operations.sh --dsh-root /path/to/deepseek-harness
bash scripts/package.sh --with-native
```

运行域测试要求 Linux root 和实际 namespace、mount、nft 能力；native 测试要求动态 OpenSSL 开发库。DSH 演示使用本机模拟模型和真实 agent，不需要云密钥。真实云验收需要有效模型服务及凭据，尚未执行，不能用 `/ready`、模型列表或模拟演示替代。

在每批提交之后建立本地审核分支，或给 PR 指定明确的前置分支；前一批合入后更新下一批 base。面向已有 AgentCensor 仓库的路径映射、CI 接线和子项目登记应作为目标仓库中的单独小提交审查。

## 本地评审分支

完整序列保存在 `feat_rust_initial`，各批次截止点保存在 `review/rust-pr01` 至 `review/rust-pr10`。这些分支只引用同一条提交链，不重复提交代码。查看第 2 批时可运行：

```bash
git log --reverse --format=full review/rust-pr01..review/rust-pr02
git diff --stat review/rust-pr01..review/rust-pr02
bash scripts/check-commit-size.sh review/rust-pr01..review/rust-pr02
```

第 1 批没有父提交，可使用 `git log --reverse review/rust-pr01` 及 `bash scripts/check-commit-size.sh review/rust-pr01`。检查脚本由文档批次引入，在完整工作树上执行即可。`git log --reverse --oneline feat_rust_initial` 可以查看全部 35 条提交，`git show <commit>` 查看对应实现与说明。将提交搬到目标仓库后以目标分支为 base，逐批更新 PR；本地 `review/` 分支不是远端 PR。

## PR 正文示例

```text
feat(gateway): add authenticated privacy model gateway

Add fixed-upstream Chat, Messages and Responses forwarding. Protect complete
requests before serialization; restore references only in passive replies.
Reject failed detection, unknown payloads and invalid authentication.

Depends on PR4. Include the complete Cargo dependency lock changes.
Validate with cargo test --locked --test http_gateway and cargo check.
Report the per-commit changed-line counts from check-commit-size.sh.
```

提交使用 Conventional Commits 和作者真实身份的 `Signed-off-by`。每条正文说明行为与验证；最终哈希及实际行数以 `git log` 和检查脚本输出为准。向评审者提供每批的检查结果，明确本机模拟、真实 DSH、系统服务与云端效果分别验证到哪一步。准备本地提交不等于已推送远端或创建 PR。
