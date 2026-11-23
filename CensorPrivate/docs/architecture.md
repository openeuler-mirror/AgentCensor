# CensorPrivacy 架构

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [使用手册](usage.md) · [检测器说明](rust-detector.md)

CensorPrivacy 是 AgentCensor 的模型数据隐私子项目。CensorFS 管理文件工作区，CensorGuard 管理内核执行与访问边界，CensorScope 管理运行观测，CensorPivot 编排批次；CensorPrivacy 负责模型请求保护、授权显示及引用计算。当前项目可独立运行，没有将自身隐式注册为其他组件的服务，也不要求修改 DSH 源码。

## 网关链路

```mermaid
flowchart LR
    Client[完整模型上下文] --> Auth[认证与严格解析]
    Auth --> Gate[协议分类与策略检测]
    Gate --> Vault[本机 vault 与随机引用]
    Vault --> Permit[序列化复查与一次性放行]
    Permit --> Provider[固定上游]
    Provider --> Restore[JSON/SSE 回复检查]
    Restore --> Display[被动显示恢复与秘密遮盖]
    Vault --> Compute[本机引用计算]
    Compute --> Derived[派生引用]
    Derived --> Restore
```

生产入口按 `detection.categories` 启用本地检测规则；选择 PERSON、ORG、LOCATION、DATE 时加载并验证 BERT，其他类别或空列表无需加载模型。HTTP 网关和 TLS broker 共用检测实例与策略。候选、验证、边界处理和 NER 聚合均在 Rust 中，NER 通过本地 ONNX Runtime 推理。资源校验或检测失败拒绝放行，每窗口推理预算默认 16 秒，具体边界见 [检测器说明](rust-detector.md)。

模型、角色、URL、工具名等结构字段不得任意改写。业务数字和协议控制数字按位置分类。放行凭据绑定目标、方法、请求字节、run 和 epoch，不能重复消费。异常只返回稳定错误码，不输出输入或上游错误正文。

只有被动显示位置可以恢复。代码、URL、Markdown 活动目标、普通工具参数与未知通道不获得显示授权。SSE 任意分片不能改变规则；未知格式与失败均拒绝，无原文回退。

## 目录与职责

| 位置 | 职责 |
| --- | --- |
| `src/professional.rs`、`src/professional/` | Rust regex/rule/验证、tokenizers、固定 BERT4NER ONNX 推理 |
| `scripts/model/` | 独立的生产模型导出、数值检查、依赖版本和产物来源 |
| `tests/fixtures/native-detector-reference.jsonl` | 共享模型导出检查输入与 Rust 固定参考结果 |
| `licenses/` | 第三方必要许可和简要来源说明；项目许可引用仓库根目录 |
| `src/policy.rs`、`detection.rs` | 不可变策略、规则与本机检测器、UTF-8 范围检查、重叠合并 |
| `src/config.rs`、`categories.rs` | 统一 YAML 配置、路径解析、类别选择与别名 |
| `src/references.rs`、`vault.rs`、`compute.rs` | 随机引用、run 授权、内存原值、期限/标签继承、精确计算 |
| `src/json.rs`、`protocols.rs`、`gate.rs` | 有界严格解析、协议字段分类、完整请求保护、一次性放行 |
| `src/display.rs`、`response.rs`、`streaming.rs` | 被动显示恢复、SECRET 遮盖、JSON/SSE 回复处理 |
| `src/server.rs`、`main.rs` | 认证、固定 HTTP/HTTPS 上游、重载与状态文件 |
| `src/mcp.rs`、`src/main.rs`、`src/cli/` | 私密工具客户端、stdio MCP、统一 censorprivacy 命令及功能子命令 |
| `src/tls_privacy.rs`、`tls_broker.rs`、`native/` | 等长映射、broker 决策、动态 OpenSSL ABI hook |
| `src/runtime/`、`crates/runtime-sys/` | Linux namespace、挂载、降权、seccomp 与必要 syscall |
| `src/private_tools.rs`、`plugins/dsh-censorprivacy/` | 本地私密工具注册与执行、DSH 模型和工具适配 |
| `scripts/ops/`、`deploy/`、`docs/` | 环境检查、安装、生命周期、演示、部署和使用说明 |
| `tests/` | 单元/集成测试、207 组固定样例、真实 native 与隔离验收 |

## 引用与授权

CP 引用格式为 `<CP_类别_8位后缀>`，后缀使用密码学随机源生成的小写字母和数字，例如 `<CP_PERSON_k7m2x9az>`。签发时检查当前 run 内是否重名，碰撞则重新生成；同一值的重复注册复用引用。引用是对象标识，原值只保存在本机 run vault。引用语法正确、知道 run ID 均不能获取读取权限；只有 `begin_run` 能构造私有字段的 `RunCapability`，访问还检查 vault 身份、所有者、run、epoch、单调时钟 TTL 和依赖。run ID、认证令牌及放行许可的长度保持原有设置。升级后需重启网关并新建 Agent 会话。

SECRET 不能恢复或计算。私密计算只接受授权引用，内部采用有界精确十进制与日期运算，结果仍是继承标签、期限和依赖的引用。表达式身份使 min/max 结果不能通过引用相等暴露获胜输入。非终止除法、混合单位、过大数值和未知操作拒绝处理。

核心 crate 使用 `#![forbid(unsafe_code)]`，自有 Linux FFI 集中在 runtime-sys；ONNX Runtime 的原生调用由第三方 ort 封装。vault 敏感字符串和运行配置释放时清零；此措施不保证所有 JSON 临时副本、第三方推理内存、大整数 limb、分配器或操作系统内存均被清除。

## TLS 与运行域

TLS direct 保持客户端原连接，hook 在加密前暂存完整请求，broker 只返回 `DEFER`、`BLOCK` 或完整等长 `REPLACE`。JSON 解码字符映射到原始 UTF-8/转义/代理对字节；别名与填充保持 wire 长度。重试仅复用已批准缓冲及偏移，绝不恢复发送原文。支持范围是动态 OpenSSL、固定 IPv4 peer、HTTP/1.1、Content-Length、非流式未压缩 JSON。

网关只保护发到其入口的请求。`censorprivacy run` 另建网络、挂载、PID/IPC、权限与 seccomp 边界，约束后代绕行。工作区必须专属，额外运行依赖显式批准。namespace 身份或人口无法确认时保留保护规则。`censorprivacy tls-run` 是协作接入，不替代强制隔离。

部署见 [构建教程](build-and-deploy.md)，日常使用与 DSH 见 [使用手册](usage.md)，可重复验证见 [验收记录](validation.md)。
