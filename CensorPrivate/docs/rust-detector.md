# Rust 隐私检测器

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [使用手册](usage.md) · [脚本速查](scripts.md)

网关、TLS broker 和检测页面共用 Rust 正则、规则与 NER。固定本地模型为 `shibing624/bert4ner-base-chinese`，使用 FP32 ONNX、Rust tokenizers 与 ONNX Runtime CPU；生产不启动 Python，不在线下载模型。

首次使用先按 [构建步骤](build-and-deploy.md#4-准备检测资源) 下载/导出资源，再按本页配置路径。模型导出细节见 [模型工具](../scripts/model/README.md)。

## 1. 资源清单

| 文件 | 用途 |
| --- | --- |
| `model.onnx` | BERT4NER FP32 权重 |
| `tokenizer.json` | 导出时使用的 fast tokenizer |
| `config.json` | BERT 配置、原始 `id2label` 顺序 |
| `manifest.json` | 模型 ID、float32、512/64 窗口、BIO 标签及前三文件 SHA-256 |
| `export-validation.json` | 导出数值报告；完整打包时使用 |
| `libonnxruntime.so` | 与平台及 ort API 匹配的原生运行库，版本文件名也可用 |

模型目录与原生库路径分别配置。安装脚本不复制这两类资源；standalone 完整包会包含并自动定位它们。

## 2. 配置路径

```bash
export CP_DETECTOR_MODEL_DIR=/path/to/onnx/bert4ner-base-chinese
export CP_DETECTOR_ORT_LIBRARY=/path/to/libonnxruntime.so
./cpctl doctor
```

`censorprivacy serve` 和 `censorprivacy web` 可用 `--detector-model-dir`、`--detector-ort-library` 覆盖对应环境变量；`cpctl` 启动网关使用环境变量。优先级如下：

| 资源 | 查找顺序 |
| --- | --- |
| 模型目录 | CLI 参数 → `CP_DETECTOR_MODEL_DIR` → 已存在的安装目录 → 编译时源码目录下 `models/bert4ner-base-chinese/` |
| ORT 库 | CLI 参数 → `CP_DETECTOR_ORT_LIBRARY` → `ORT_DYLIB_PATH` → 安装布局中的库路径 |

安装布局以可执行文件上两级为 `<prefix>`：

```text
<prefix>/
├── bin/censorprivacy
├── lib/censorprivacy/libonnxruntime.so
└── share/censorprivacy/models/bert4ner-base-chinese/
```

源码二进制位于 `target/debug/` 或 `target/release/` 时，默认 ORT 位置相应为 `target/lib/censorprivacy/libonnxruntime.so`。该目录被 Git 忽略，`cargo clean` 后需重新准备；服务不会自动搜索 Python 环境。

CLI 与环境变量只定位固定模型，不提供模型选择。`--model` / `init --model-id` 指上游聊天模型，与本地 NER 资源无关。变量不由 `init` 持久保存；systemd 安装会把当前变量写入实例环境文件，部署步骤见 [系统服务](build-and-deploy.md#6-部署为系统服务)。

## 3. 验证资源与运行

```bash
# 无模型的规则与资源拒绝测试
bash scripts/test/detector.sh
# 真实 ONNX：固定参考结果、网关/SSE、共享 TLS 转换与 HTTPS CLI
bash scripts/test/detector.sh --integration
```

启动时验证 manifest、文件哈希、BERT 类型、标签顺序、ONNX 输入输出，并实际预热推理；失败不开放服务。哈希用于资源一致性检查，资源来源由部署者确认。

Rust 依赖锁定 ort `2.0.0-rc.10`、tokenizers `0.21.4`；本机验收使用 ONNX Runtime `1.31.1 CPU / Linux x86_64`。其他平台/版本需按同样链路验收。原生 OpenSSL 和浏览器验收另见 [脚本速查](scripts.md#4-测试与验收)。

## 4. 检测行为与边界

`src/professional.rs` 负责检测接口和失败状态；`professional/model.rs` 负责 tokenizer、ONNX 和 BIO 聚合；`professional/rules.rs` 负责正则、规则、上下文与验证，`catalog.json` 保存版本管理的生产规则数据。

规则与候选处理已对齐上游 `privacy-detector` 的 `007c7ea7b5e4c58085564cc336ffd75f28b73d74`（其直接父提交为 `d4927f5e06d3d4f90667b08926ef5df1568ee9d7`）。原迁移已包含部分地址、车牌和验证逻辑，本次补齐短地址及联系字段边界、上下文车牌/卡号、分组身份证、护照格式、邮箱内人名过滤、证件/卡号冲突消解和独立的验证状态合并；没有引入上游未提交文件或 Python 运行依赖。

`censorprivacy web` 提供同一检测器的本地可视化入口，页面来源及与生产网关的区别见 [隐私检测台](../plugins/detector-web/README.md)。

- 正则/规则覆盖邮箱、电话、身份证、银行卡、账号、密码/API 凭据、IP、车牌、地址、金额、日期等。NER 补充人名、机构、位置和时间；不引入第二个语义模型。
- 身份证检查出生日期和 GB11643，银行卡检查 Luhn，IP 使用标准库，车牌检查格式。无验证器的候选标记 not_checked；有上下文的无效证件/卡号按原语义以 0.6 保守保护。
- NER 使用原始 FP32 softmax 分数、simple BIO 聚合、512 token 窗口及 64 token 重叠，处理所有 overflow 窗口。候选阈值 0.5，网关阈值 0.6，比较包含等号。所有对外区间为原文 UTF-8 字节范围。
- 字段别名、边界补全、重复值、验证状态和 evidence 保留。已有 CP 引用是检测边界；不属于本次 vault 的输入引用仍由网关转义为 LITERAL。
- 金额保留数值范围和币种，便于私密计算；2万元等缩放表达式保守保护为 CONFIDENTIAL。网关、SSE、TLS 等长转换、vault 和恢复接口沿用原实现，SECRET 不恢复。
- Rust policy 的 dictionaries、patterns、标签、金额/日期/全数字开关及 epoch reload 继续生效；额外检测器可通过原 Detector trait 注入。内置专业规则当前编译进 catalog，修改后需重建；模型固定。
- CLI 默认加载专业检测器。Rust 嵌入方使用 `PrivacyGate::with_professional_detector`，或共享 Arc 的 `with_loaded_professional_detector`。`PrivacyGate::new`、`with_detectors`、Detector/LocalAnalyzer 保留原轻量接口，不隐式加载 BERT。

单次文本上限 1 MiB、字段名上限 8192 bytes，正则有回溯预算，候选上限 65536。推理串行运行；每次 predict 设置 60 秒计时并请求 ONNX Runtime cooperative termination，超时结果拒绝，检测错误锁存为失败。它不是进程级强制中断，也不是多字段请求的统一 60 秒截止时间。TLS hook 本机 RPC 保留 60 秒有限等待。长文和多字段生产容量需单独评测。

检测器不建立第二份原值映射表，不记录输入原文；原值/引用仍由 vault 持有。重复分析使用最多 512 项、合计 4096 个候选的内存缓存，只存请求 SHA-256 和位置/元数据，键包含字段、structural 和三个策略开关；健康状态先于缓存检查，错误后清空并拒绝继续。扫描副本使用 Zeroizing，第三方 tokenizer、ONNX Runtime、分配器和其他临时副本不保证安全清零。

## 5. 源码与资源管理

| 位置 | 职责 | 是否进入 Git |
| --- | --- | --- |
| `src/professional.rs`、`src/professional/` | 产品检测接口、规则、验证、tokenizer 与 ONNX 推理 | 是 |
| `scripts/model/` | 导出工具、直接依赖版本、已验收模型来源与哈希 | 是 |
| `tests/fixtures/native-detector-reference.jsonl` | 导出输入与 Rust 必要回归共用的 59 组样本 | 是 |
| `licenses/` | 检测代码、模型与第三方许可来源 | 是 |
| `models/`、源权重、运行库、虚拟环境 | 本地准备或随独立交付包分发的资源 | 否 |
| `offline/` | 完整 Python 迁移与对照资料，仅本地保留 | 否 |

公开构建、模型导出和必要回归均使用仓库内文件，不依赖 `offline/`。固定合成样本验证迁移与回归，不能证明任意业务召回率。迁移来源和实际验证结果见 [验收记录](validation.md)。
