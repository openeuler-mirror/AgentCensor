# 隐私检测台

复用 `privacy-detector` 提交 `007c7ea7b5e4c58085564cc336ffd75f28b73d74` 的页面、样式和交互。后端使用产品的 Rust `ProfessionalDetector`，固定本地 `shibing624/bert4ner-base-chinese` FP32 ONNX，不需要 Flask、Python、Node 或前端构建。

先按 [检测器说明](../../docs/rust-detector.md) 准备模型与 ONNX Runtime，再从 CensorPrivate 目录启动：

```bash
cargo run --locked --bin censorprivacy -- web \
  --detector-model-dir /path/to/onnx/bert4ner-base-chinese \
  --detector-ort-library /path/to/libonnxruntime.so
```

也可使用已安装的 `censorprivacy web`，或 `target/release/censorprivacy web`；资源路径与网关共用 `CP_DETECTOR_MODEL_DIR`、`CP_DETECTOR_ORT_LIBRARY` 环境变量及安装布局。浏览器打开终端输出的 `http://127.0.0.1:8766/`；`--port` 可更换端口。模型校验及 warmup 成功后才监听，Ctrl+C 停止。

源码构建不会下载或生成 ONNX Runtime。模型已在默认的 `models/bert4ner-base-chinese/` 时，可以只传运行库路径：

```bash
CP_DETECTOR_ORT_LIBRARY=/path/to/libonnxruntime.so \
  cargo run --locked --bin censorprivacy -- web
```

也可将准备好的原生库放到 `target/lib/censorprivacy/libonnxruntime.so`（使用自定义 Cargo target 目录时相应调整），供 `target/debug/` 和 `target/release/` 下的命令自动加载。该本地文件不进入 Git，`cargo clean` 后需重新准备。缺少模型文件或运行库时，启动会提示对应参数；仅编译成功不代表检测资源已就绪。

保留原文高亮、结果定位、来源/校验状态、遮蔽分数阈值、三种预览和复制功能。正则与精确规则始终启用，NER 可以关闭；移除未提供的 spaCy/语义模型选择。中文 BERT 不提供英文专用 NER，英文示例可检查正则和规则。

输入框默认空白，示例只在主动点击示例按钮后载入。

页面显示生产检测结果，并额外展示普通数字的 0.4 分线索；NER 沿用生产输出阈值 0.6。遮蔽阈值默认 0.6，包含等号。界面标注使用 Unicode 字符区间，Rust 检测接口仍使用 UTF-8 字节区间。重叠范围复用产品的合并逻辑。

`./cpctl start` 还会在网关端口（默认 `http://127.0.0.1:8775/`）提供同一检测台，并与网关共用已加载模型。运行 `./cpctl web` 可查看地址。通过 `./cpctl dsh run`、`./cpctl dsh web` 或 `./cpctl xiaoo tui --xiaoo-root /path/to/xiaoO` 发起对话后，页面自动按 Agent 来源与会话同步对话过程、流式回复和工具交互。DeepSeek Harness 插件自动读取每次模型调用的真实 `sessionId`，辅助标题/压缩调用不混入对话。点击消息下的按钮复用原文标注、结果列表和遮蔽视图，展示当次真实网关结果；这些记录为只读，不会重新推理或生成新引用。切回独立检测后可继续手动提交文本。

底层 `censorprivacy serve --conversation-records-dir /绝对路径` 启用记录和网页，仅允许 IPv4 loopback、非 TLS 监听。旧参数 `--xiaoo-records-dir`、旧 `/xiaoo/<32位小写十六进制会话标识>/...` 路径和历史记录继续兼容。`cpctl` 保留原 `xiaoo/records/` 存储路径，避免丢失已有记录。独立的 `censorprivacy web` 可用同名参数读取该目录。

其他 OpenAI Chat Completions 客户端可选择以下一种接入方式：

- 请求头：`X-CensorPrivacy-Agent: my-agent` 与 `X-CensorPrivacy-Session: session-001`，请求仍发往 `/v1/chat/completions`。
- Base URL 参数：`http://127.0.0.1:8775/agents/my-agent/session-001/v1`，客户端按常规追加 `/chat/completions`。需要完整端点的客户端使用 `/agents/my-agent/session-001/v1/chat/completions`。

来源最多 64 字符，会话最多 128 字符，仅接受 ASCII 字母、数字、`-_.:`；同一会话保持相同标识，新会话更换标识。网关按来源与会话生成独立本机 ID，并在转发前移除这些传输参数，不发送给上游。两种方式不能同时使用。未提供标识的普通请求不记录，避免将并发 Agent 猜测合并。旧版 Harness 没有 `sessionId` 时，可在插件配置中显式设置 `conversationSession`，每个会话使用独立配置。当前记录器适用于 gateway 模式的 Chat Completions；TLS direct、Messages/Responses 协议及接入前的旧 Harness 历史不会被自动导入。更新后重启网关和 Harness 客户端即可开始同步新请求。

校验 Host/Origin，响应禁用缓存，不写浏览器存储。独立检测不向上游发送文本、不保存输入；独立预览不加载网关自定义 policy、vault 或恢复状态，预览 CP 引用不能用于网关恢复。Agent 记录包含原文并保存到本机私有目录（0700/0600），刷新与网关重启后可回看，不自动清理。右上角“清空记录”经确认后永久删除当前实例全部会话，清除页面中的原文和检测详情；进行中的旧记录不会重新落盘。清空不改变 Agent 对话上下文，新一轮测试应新建 Agent 会话。接口为同源 `DELETE /api/conversations`，要求 JSON `{"confirm":true}`，与读取、录制共用文件锁，支持独立 Web 与网关读取同一目录。回复展示本地恢复后的内容；其下一轮再次发送的版本带有单独的检测记录。页面资源编译进原有 `censorprivacy` 命令，安装后无需复制静态文件。

通用 Agent 网页验收复用 [dsh-web.mjs](../../scripts/test/dsh-web.mjs)，覆盖真实 Harness CLI/Web 与检测台同步、通用 Base URL 和混合来源展示。

必要回归：`CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 cargo test --locked --lib --test http_gateway`。独立检测浏览器验收使用 [detector-web.mjs](../../scripts/test/detector-web.mjs)；真实 xiaoO 与网页端到端验收使用 [xiaoo-web.mjs](../../scripts/test/xiaoo-web.mjs)，参数为网关二进制和 xiaoO 二进制，需本地模型、Playwright/Chromium；测试依赖不参与产品运行。
