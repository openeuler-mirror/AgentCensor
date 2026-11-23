# Rust 隐私检测器

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [使用手册](usage.md) · [脚本速查](scripts.md)

网关、TLS broker 和检测页面共用 Rust 正则、规则与 NER。固定本地模型为 `shibing624/bert4ner-base-chinese`，使用 FP32 ONNX、Rust tokenizers 与 ONNX Runtime，支持 CPU、NVIDIA CUDA 或华为昇腾 CANN；生产不启动 Python，不在线下载模型。

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
./censorprivacy doctor
```

`censorprivacy serve` 和 `censorprivacy web` 可用 `--detector-model-dir`、`--detector-ort-library` 覆盖对应环境变量；也可通过 [config.yaml](configuration.md) 的 `ner.model_dir`、`ner.ort_library` 配置资源，YAML 优先于环境变量。优先级如下：

| 资源 | 查找顺序 |
| --- | --- |
| 模型目录 | CLI 参数 → YAML `ner.model_dir` → `CP_DETECTOR_MODEL_DIR` → 已存在的安装目录 → 编译时源码目录下 `models/bert4ner-base-chinese/` |
| ORT 库 | CLI 参数 → YAML `ner.ort_library` → `CP_DETECTOR_ORT_LIBRARY` → `ORT_DYLIB_PATH` → 安装布局中的库路径 |

安装布局以可执行文件上两级为 `<prefix>`：

```text
<prefix>/
├── bin/censorprivacy
├── lib/censorprivacy/libonnxruntime.so
└── share/censorprivacy/models/bert4ner-base-chinese/
```

源码二进制位于 `target/debug/` 或 `target/release/` 时，默认 ORT 位置相应为 `target/lib/censorprivacy/libonnxruntime.so`。该目录被 Git 忽略，`cargo clean` 后需重新准备；服务不会自动搜索 Python 环境。

NER 并行度可用 `censorprivacy serve/web --ner-threads N` 或环境变量 `CP_NER_THREADS=N` 配置，优先级为命令行 → YAML → 环境变量 → 默认 1，范围 1–256；inter-op 固定为 1。`censorprivacy init --ner-threads N` 持久保存到 `config.json`，`censorprivacy start --ner-threads N` 临时覆盖，未指定时使用 YAML 的 `ner.threads`，再回退到实例保存值或 1。`censorprivacy` 使用实例值作为环境回退，因此不继承终端的 `CP_NER_THREADS`；4 核 CPU 可选择 4，需按实际负载验证。认证 `/ready` 会返回生效的 `ner_threads`。

设备和 batch 由检测核心统一处理，适用于所有 Harness/协议：

```bash
# CPU：保留 ONNX CPU 算子线程配置
./censorprivacy start --ner-device cpu --ner-threads 4
# CUDA：默认第 0 张可见卡，每卡每次最多 4 个窗口
./censorprivacy start --ner-device cuda --ner-batch-size 4
# 多卡：每卡一个会话，空闲卡继续领取下一批，结果按文档顺序合并
./censorprivacy start --ner-device cuda:0,1,2,3 --ner-batch-size 4
# 华为昇腾：默认第 0 张卡，也支持 cann:0,1 多卡列表
./censorprivacy start --ner-device cann --ner-batch-size 4
```

`--ner-device` 默认 `cpu`，`cuda` 等同于 `cuda:0`，`cann` 等同于 `cann:0`；两种后端均支持多卡列表且禁止重复，编号对应进程可见设备（CUDA 受 `CUDA_VISIBLE_DEVICES` 影响，CANN 按其运行环境的可见设备设置）。`--ner-batch-size` 范围 1–256，默认 1，按每卡计算。`init` 可保存这两个参数，`start` 临时覆盖且不改写配置；旧配置仍使用 CPU/batch 1。直接运行 `censorprivacy serve/web` 时可通过 `CP_NER_DEVICE`、`CP_NER_BATCH_SIZE` 设置默认值，CLI 优先于 YAML，再回退到环境变量；`censorprivacy` 使用 YAML 优先、实例配置回退，不继承终端的这两个环境变量。认证 `/ready` 返回规范化的 `ner_device`、`ner_batch_size` 和原有 `ner_threads`。

batch 只组合当前文本的窗口，不等待其他请求；最后不足一批立即推理，按每批最长窗口右侧补齐并使用 attention mask，补齐部分不参与实体聚合。多卡每卡至多一个在途 batch，所有卡均在启动时加载模型并验证推理；任何一批失败拒绝整次结果。`--ner-threads` 仍控制各会话的 CPU 算子线程，不代表 GPU 分发线程数。多卡会增加模型副本和主机内存占用；短文本窗口不足时不会用满所有卡。

当前 standalone 包附带 **CPU** 运行库。CUDA 使用同一个 Rust 二进制，但需要另外准备与 ort API、驱动及 CUDA/cuDNN 匹配的 ONNX Runtime GPU 原生库，在 YAML 的 `ner.ort_library` 中填写路径；同目录的 CUDA provider 库及其依赖必须能被动态加载器找到。将该字段设为 `null` 时也可使用 `CP_DETECTOR_ORT_LIBRARY`。Rust 服务不需要安装 Python 的 `onnxruntime-gpu` 包。CUDA 显式关闭 TF32，保持 FP32 推理精度策略。显式选择加速设备后，provider 注册或会话创建失败返回 `ner_device_unavailable`；部分不受选定 provider 支持的算子仍可由 ORT 分配给 CPU。

昇腾使用同一二进制接口和 batch/多卡调度层，但需要 **含 CANNExecutionProvider 的 ONNX Runtime**、匹配的 Ascend 驱动/固件及 CANN Toolkit。普通 CPU/CUDA ORT 库不自动支持 CANN。通过 `ner.ort_library` 指定库，并按 CANN 安装说明配置动态库搜索路径。CANN 保留模型原始精度（`must_keep_origin_dtype`），优先高精度实现；不自动转换为 FP16。现有交付包为 x86_64，ARM 昇腾主机需要另行构建对应架构的二进制和运行库。

加速卡主机可运行独立验收（同一加速运行库先跑 CPU 参考、释放模型后再跑指定设备，比较 batch 1/4 的实体、偏移和分数；昇腾将设备改为 `cann` 或 `cann:0,1`）：

```bash
CP_TEST_NER_DEVICE=cuda:0,1 \
CP_DETECTOR_ORT_LIBRARY=/path/to/gpu/libonnxruntime.so \
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 \
cargo test --locked --test ner_accelerator -- --ignored --nocapture --test-threads=1
```

CUDA/CANN 的真实设备执行、算子覆盖、数值差异和性能须在对应硬件主机验收。本机 CPU 回归和后端不可用检查不代表真实加速硬件已通过验收。

模型路径的 CLI 与环境变量只定位固定模型，不提供模型选择。`--model` / `init --model-id` 指上游聊天模型，与本地 NER 资源无关。变量不由 `init` 持久保存；systemd 安装会把当前变量写入实例环境文件，部署步骤见 [系统服务](build-and-deploy.md#6-部署为系统服务)。

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
- 在统一配置的 `custom` 中填写字典、正则和标签，通过 `custom.epoch` 热重载。金额、日期、普通数字和其他类别统一通过 `detection.categories` 选择。额外检测器可通过原 Detector trait 注入。内置专业规则当前编译进 catalog，修改后需重建；模型固定。
- CLI 默认加载专业检测器。Rust 嵌入方使用 `PrivacyGate::with_professional_detector`，或共享 Arc 的 `with_loaded_professional_detector`。`PrivacyGate::new`、`with_detectors`、Detector/LocalAnalyzer 保留原轻量接口，不隐式加载 BERT。

单次文本上限 1 MiB、字段名上限 8192 bytes，正则有回溯预算，候选上限 65536。请求按检测器互斥访问，单卡逐批运行、多卡可并行运行同一文本的不同批次。每个 512 token 窗口的 ONNX 执行预算默认为 16 秒（可通过 `ner.window_timeout_seconds` 调整）：默认单窗执行时逐窗计时；一批包含多个窗口时，ONNX 将该批作为一次执行，预算为实际窗口数乘以配置的每窗秒数，尾批也按实际数量计时。排队不消耗后续批次的预算。超时请求 ONNX Runtime 协作中止，整次检测拒绝返回部分结果并锁存失败。TLS hook 本机 RPC、DSH 客户端和上游 HTTP 仍有各自的传输超时；长文吞吐要按实际硬件评估。

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
