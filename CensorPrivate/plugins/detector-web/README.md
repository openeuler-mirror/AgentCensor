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

页面显示生产检测结果，并额外展示普通数字的 0.4 分线索；NER 沿用生产输出阈值 0.6。遮蔽阈值默认 0.6，包含等号。界面标注使用 Unicode 字符区间，Rust 检测接口仍使用 UTF-8 字节区间。重叠范围复用产品的合并逻辑。

仅监听 IPv4 loopback，校验 Host/Origin；不向上游发送文本，不写入日志或浏览器存储，响应禁用缓存。页面是独立检测预览，不加载网关自定义 policy、vault 或恢复状态；预览 CP 引用不能用于网关恢复。页面资源编译进原有 `censorprivacy` 命令，安装后无需复制静态文件。

必要回归：`cargo test --locked --bin censorprivacy`。真实模型浏览器验收使用 [detector-web.mjs](../../scripts/test/detector-web.mjs)，需本地 Playwright/Chromium；不参与产品运行。
