# 隐私检测台

复用 `privacy-detector` 提交 `007c7ea7b5e4c58085564cc336ffd75f28b73d74` 的页面、样式和交互。后端使用产品的 Rust `ProfessionalDetector`，固定本地 `shibing624/bert4ner-base-chinese` FP32 ONNX，不需要 Flask、Python、Node 或前端构建。

选择 PERSON、ORG、LOCATION、DATE 任一类时，先按 [检测器说明](../../docs/rust-detector.md) 准备模型与 ONNX Runtime；四类全关时无需这些资源。从 `CensorPrivate/` 目录启动：

```bash
./target/release/censorprivacy web
```

安装后使用 `censorprivacy web`；完整包使用 `./bin/censorprivacy web`。默认读取当前目录的 `config.yaml`，也可用 `--config FILE`，其中 `ner` 和 `detection.categories` 与网关含义一致。浏览器打开终端输出的 `http://127.0.0.1:8766/`；`--port` 可更换端口，Ctrl+C 停止。

源码构建不准备模型或 ONNX Runtime。资源可通过 `ner.model_dir`、`ner.ort_library` 持久配置，也支持 CLI、环境变量与安装布局，优先级见 [检测器配置](../../docs/rust-detector.md#2-配置路径)。选择需要 NER 的类别时，启动会校验模型并预热；缺少资源则拒绝启动。

页面提供原文高亮、结果定位、来源/校验状态、遮蔽分数阈值、三种预览和复制功能。检测范围由 `detection.categories` 选择，页面还可关闭本次 NER。中文 BERT 不提供英文专用 NER，英文示例可检查已启用的正则和规则。预览 CP 引用使用 8 位小写字母和数字后缀，同一类别的重复值复用引用。

输入框默认空白，示例只在主动点击示例按钮后载入。

当前内置 16 类。ADDRESS 只保护有通讯/邮寄结构的地址，不因通用 `address` 字段名而覆盖 IP、URL、域名或文件路径；IP_ADDRESS 独立检测 IPv4/IPv6。MEDICAL_INFO 及医疗标签检测已移除，页面改用通讯地址与寄送回访示例，不提供医疗语义检测。

页面显示生产检测结果，并额外展示普通数字的 0.4 分线索；NER 沿用生产输出阈值 0.6。遮蔽阈值默认 0.6，包含等号。界面标注使用 Unicode 字符区间，Rust 检测接口仍使用 UTF-8 字节区间。重叠范围复用产品的合并逻辑。

`./censorprivacy start` 还会在网关端口（默认 `http://127.0.0.1:8775/`）提供同一检测台，并与网关共用已加载模型。运行 `./censorprivacy web` 可查看地址。通过 `./censorprivacy dsh run`、`./censorprivacy dsh web` 或 `./censorprivacy xiaoo tui --xiaoO-path /path/to/xiaoO` 发起对话后，页面自动按 Agent 来源与会话同步对话过程、流式回复和工具交互。DeepSeek Harness 插件自动读取每次模型调用的真实 `sessionId`，辅助标题/压缩调用不混入对话。点击消息下的按钮展示当次真实网关结果；历史详情只读，不重新推理或生成引用。

底层 `censorprivacy serve --conversation-records-dir /绝对路径` 启用记录和网页，仅允许 IPv4 loopback、非 TLS 监听。旧参数 `--xiaoo-records-dir`、旧 `/xiaoo/<32位小写十六进制会话标识>/...` 路径和历史记录继续兼容。`censorprivacy` 保留原 `xiaoo/records/` 存储路径，避免丢失已有记录。独立的 `censorprivacy web` 可用同名参数读取该目录。

其他 OpenAI Chat Completions 客户端可选择以下一种接入方式：

- 请求头：`X-CensorPrivacy-Agent: my-agent` 与 `X-CensorPrivacy-Session: session-001`，请求仍发往 `/v1/chat/completions`。
- Base URL 参数：`http://127.0.0.1:8775/agents/my-agent/session-001/v1`，客户端按常规追加 `/chat/completions`。需要完整端点的客户端使用 `/agents/my-agent/session-001/v1/chat/completions`。

来源最多 64 字符，会话最多 128 字符，仅接受 ASCII 字母、数字、`-_.:`；同一会话保持相同标识，新会话更换标识。网关按来源与会话生成独立本机 ID，并在转发前移除这些传输参数，不发送给上游。两种方式不能同时使用。未提供标识的普通请求不记录，避免将并发 Agent 猜测合并。旧版 Harness 没有 `sessionId` 时，可在插件配置中显式设置 `conversationSession`，每个会话使用独立配置。当前记录器适用于 gateway 模式的 Chat Completions；TLS direct、Messages/Responses 协议及接入前的旧 Harness 历史不会被自动导入。更新后重启网关和 Harness 客户端即可开始同步新请求。

校验 Host/Origin，响应禁用缓存，不写浏览器存储。独立检测不向上游发送文本、不保存输入；独立预览不应用 `custom` 业务规则或 vault 恢复，预览引用不能用于网关。Agent 记录包含原文并保存到本机私有目录（0700/0600），刷新与网关重启后可回看，不自动清理。右上角“清空记录”经确认后永久删除当前实例全部会话；进行中的旧记录不会重新落盘。清空不改变 Agent 对话上下文，新一轮测试应新建 Agent 会话。接口为同源 `DELETE /api/conversations`，要求 JSON `{"confirm":true}`，与读取、录制共用文件锁，支持独立 Web 与网关读取同一目录。回复展示本地恢复后的内容；下一轮再次发送时另存当次检测记录。页面资源内嵌二进制，无需单独安装静态文件。

通用 Agent 网页验收复用 [dsh-web.mjs](../../scripts/test/dsh-web.mjs)，覆盖真实 Harness CLI/Web 与检测台同步、通用 Base URL 和混合来源展示。

必要回归：`CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 cargo test --locked --lib --test http_gateway`。独立检测浏览器验收使用 [detector-web.mjs](../../scripts/test/detector-web.mjs)；真实 xiaoO 与网页端到端验收使用 [xiaoo-web.mjs](../../scripts/test/xiaoo-web.mjs)，参数为网关二进制和 xiaoO 二进制，需本地模型、Playwright/Chromium；测试依赖不参与产品运行。
