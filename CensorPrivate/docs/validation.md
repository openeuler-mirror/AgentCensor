# Rust 运维与演示验收记录

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [使用手册](usage.md) · [测试入口](scripts.md#4-测试与验收)

本页按日期保存实测证据，历史记录保留当时版本与范围；它不是首次运行教程。当前命令以构建、使用和脚本说明为准。

| 查阅内容 | 入口 |
| --- | --- |
| 当前 detector 与可视化更新 | [2026-11-09](#2026-11-09-detector-更新与可视化入口) |
| 公开源码收敛与导出复核 | [2026-11-09 源码复核](#2026-11-09-仓库内容收敛) |
| 完整包平台与交付验收 | [2026-11-05 完整包](#2026-11-05-openeuler-完整二进制包验收) |
| Rust + ONNX 迁移 | [2026-11-05 迁移](#2026-11-05-rust--onnx-迁移验收) |
| Python worker 与更早实现 | [历史版本记录](#历史-python-worker-版本验收featcensorprivate-pr10) |

## 2026-11-09 detector 更新与可视化入口

已确认上游 `007c7ea7b5e4c58085564cc336ffd75f28b73d74` 的直接父提交为 `d4927f5e06d3d4f90667b08926ef5df1568ee9d7`。只使用 Git 提交快照，将缺失的检测规则、边界处理及校验逻辑迁入 Rust；上游工作区未提交修改未参与。页面位于 `plugins/detector-web/`，由原有 `censorprivacy web` 命令内嵌提供。

- 格式检查、Clippy `-D warnings`、Rust workspace 默认测试通过；包含无模型规则回归及 Web 的 Unicode、重叠范围、阈值与本机请求边界测试。
- 59 组固定样本经最新上游规则核对，8 组因新规则、来源/evidence 或分数变化而更新；沿用冻结的 NER 参考值。真实 Rust ONNX 检测结果全部匹配，分数容差为 `2e-5`。
- `professional_gateway` 的 3 项真实模型测试及 `https_cli` 的 HTTPS 测试通过，覆盖参考样本、私密计算、HTTP/SSE、共享 TLS 转换、恢复与重载。
- `operations.sh --static` 的语法检查及 8 项 DSH 插件测试通过。`scripts/test/detector-web.mjs` 使用真实 ONNX 与 Chromium，通过中文/emoji 高亮、地址边界、上下文无效证件保护、遮蔽阈值、手机布局、输入验证及 Host/Origin 拒绝检查。

本机编译、真实模型测试、浏览器验收串行运行，Cargo 构建任务和测试线程均限制为 1。本轮未重新打包完整交付归档，未重跑需要 root 运行域或 C/OpenSSL hook 的扩展验收；没有云模型调用。复现入口见 [检测器说明](rust-detector.md) 和 [隐私检测台](../plugins/detector-web/README.md)。

## 2026-11-09 仓库内容收敛

公开源码保留 Rust 产品、scripts/model/ 模型导出工具及固定回归样本；完整 Python 迁移资料仅在本地保留。Cargo 和公开测试入口不再包含迁移差分 runner。模型准备按 [模型工具](../scripts/model/README.md) 执行，默认回归使用 bash scripts/test/run-all.sh，真实 ONNX 集成使用 bash scripts/test/detector.sh --integration。

按 Git 可提交文件生成不含 offline/、模型或虚拟环境的独立源码副本，复用本机 Cargo 依赖和构建缓存验证：fmt、Clippy、Rust 默认回归（65 passed、5 ignored）、8 项插件测试和 3 项真实 ONNX 集成通过。使用已有 Python 环境及校验过的五个源模型文件，从新路径完整导出通过，63 窗口标签一致，三个产物哈希与验收快照相同；本轮未测试全新联网依赖安装。打包器用合成小文件验证了新目录的模型元数据收集，未重新生成完整交付包。

## 2026-11-05 DeepSeek Flash 与 Harness 实际接口兼容

本机 openEuler 联网环境下，deepseek-flash 默认返回非空 reasoning_content，原网关按既有协议限制返回 protocol_blocked；显式关闭 thinking 后实际模型请求通过。运维探测和 DSH 插件已对 deepseek-* 模型加入 thinking: {"type":"disabled"}。插件接受非流式 tool_calls 中合法的 index 元数据，并在工具 schema 和说明中要求完整引用格式。

真实两轮工具调用还复现了 NER 将孤立的 sum 识别成人名，导致后续工具枚举被拒绝。网关现对 private_compute/privcalc 的固定 operation 按协议字段校验，其他参数继续检测；真实 ONNX 两轮回归、10 项网关回归、6 项插件回归、Clippy 和格式检查通过。最终完整包独立解压后的 11 项检查通过，包含网关和实际 OpenSSL TLS。使用该包和已有私有凭据执行 DeepSeek Harness 云端验收通过：一次引用求和、引用结果、客户名和 300.00 恢复、SECRET 遮盖；不是云端内部存储审计。

已更新安装目录并重启网关，保留连接配置与凭据。报告位于私有配置目录 dsh/cloud-report.json；脱敏验收摘要和最终归档 SHA-256 位于 dist/ 的 acceptance.json，详细日志位于 .artifacts/deepseek-compatibility/。测试入口使用独立配置和本次构建的二进制，避免旧版系统安装干扰；测试命令等待上限按实际 NER 与 Harness 启动时间调整。以下完整包初次验收记录保留其原始范围。

## 2026-11-05 openEuler 完整二进制包验收

按本机 openEuler 24.03 LTS、x86_64、glibc 2.38 的联网环境验收；没有使用容器或断网条件。完整归档包含五个 Rust 命令、TLS hook、固定 FP32 ONNX 模型、ONNX Runtime CPU、Node、运维工具、依赖库及许可。打包时生成内部 SHA256SUMS 和外部归档校验文件，ELF 使用相对库路径。

独立解压到 .artifacts/standalone-validation/unpacked/，使用包内 Node 从另一临时工作目录运行 scripts/test/standalone.mjs；不传入检测器环境变量。11 个验收检查通过，覆盖五个命令帮助、自检（网关和真实 OpenSSL TLS）、配置/凭据路由、中文人名/机构/手机号/金额/SECRET 的脱敏恢复、认证、重启 run ID 和停止监听。自检上游为本机模拟服务，不代表外部云模型验收。原有安装方式的 4 项运维测试也通过；格式和差异检查通过。

交付位于忽略目录 dist/，使用方法见 [二进制包说明](binary-distribution.md) 和包内 README.md。归档 SHA-256 对应的验收报告、精简说明和 .sha256 文件随交付保存；详细日志位于 .artifacts/standalone-validation/。该包不包含 Python、Rust 源码、虚拟环境或本机凭据。privacy-run 与 DSH 的额外系统要求见说明，它们不计入本次网关/TLS 开箱验收。

## 2026-11-05 Rust + ONNX 迁移验收

基线为更新后的 origin/master 123c31fb2b8aafaa878faa681356795e17c0afc2，工作分支 feat_private。唯一 NER 为 shibing624/bert4ner-base-chinese；生产没有 Python worker 或回退。本节保留迁移时的验收结果；当时使用的完整 Python 对照实现现仅在本地保留，不随仓库分发。

| 项目 | 本轮实测 |
| --- | --- |
| FP32 ONNX 导出 | 63 窗口 token argmax 全部一致；max logits error=9.775161743164062e-6，max probability error=2.2649765014648438e-6 |
| Rust / Python 差分 | 59 组输入，NER、最终候选类型/UTF-8 字节范围、校验/来源/证据/单位均一致；max entity score error=2.980232238769531e-7，容差 2e-5；候选脱敏及实际 PrivacyGate 脱敏无差异 |
| 离线 oracle | 159 项 Python 单测通过；生产检测不调用该解释器 |
| 原生规则与拒绝 | 无模型/无 Python 的 25 条 structural 对照通过；身份证日期/校验位、年份 0 拒绝、Luhn 回归通过；缺失/错误模型 ID/量化标记/标签/文件哈希/篡改资源均拒绝 |
| Rust 回归 | workspace/all-targets：64 passed / 0 failed，4 项环境相关测试默认忽略（其中网关、HTTPS、native TLS 已另行实跑）；fmt、Clippy -D warnings 通过 |
| HTTP/SSE 与共享 TLS | 真实 ONNX 检测实例通过 Chat、Messages、Responses、SSE、长文跨窗、Unicode、结构字段、占位符恢复、SECRET 遮盖、计算、reload 与 TLS 等长转换 |
| HTTPS CLI | 实际 Rust 服务进程、HTTPS 监听和证书验证、HTTPS 上游、CLI 引用计算、SIGHUP 重载通过 |
| 原生 OpenSSL hook | 真实 ONNX 检测通过 64 KB 请求、动态 OpenSSL、分片、非阻塞重试、curl、计算和恢复；保持 launcher 原有 30 秒限制 |
| 安装运维 | release 构建通过；4 项 Node 验收全部通过，含临时前缀安装、启停/重启、认证、网关和真实 TLS 演示；安装包含检测器许可证/新文档，不包含 Python worker |

本次数字来自固定合成回归样本，用于验证移植一致性，不是业务召回率或性能基准。每条差分样本使用独立 vault；输入中已有的 CP 语法按原网关语义转义为 LITERAL，映射表已知值保护保持不变。下文 Python/旧版记录不作为当前 Rust + ONNX 的通过证据。

本机使用 Rust 1.85.1、ONNX Runtime 1.31.1 CPU；导出使用 torch 2.14.1+cpu、onnx 1.23.2。完整准备和复现步骤见 [Rust 检测器说明](rust-detector.md)。本轮日志、parity.json 位于忽略目录 .artifacts/native-validation/；export-validation.json 位于 models/bert4ner-base-chinese/。

~~~bash
export CP_DETECTOR_MODEL_DIR=/path/to/onnx/bert4ner-base-chinese
export CP_DETECTOR_ORT_LIBRARY=/path/to/libonnxruntime.so
bash scripts/test/run-all.sh
bash scripts/test/detector.sh --integration
CP_TEST_REAL_DETECTOR=1 bash scripts/test/native.sh
bash scripts/test/operations.sh
~~~

## 2026-11-05 源码分层与复现复核

本节记录当时包含 Python 迁移对照的分层复核。当前模型准备入口已迁移至 scripts/model/，完整 Python 对照、评测和差分工具仅在本地保留，许可与来源位于 licenses/README.md。默认 Rust 构建不导入或启动 Python。当时未重新执行完整 ONNX 导出，使用此前已验收产物，并核对源权重 SHA-256；依赖版本、源文件/产物哈希与导出数值报告保留在模型工具目录中。

| 检查 | 实测结果 |
| --- | --- |
| 构建边界 | 当时 Cargo 元数据确认默认五个生产命令，另行显式启用迁移差分 runner；该 runner 现仅保留在本地 |
| 源码副本 | 将 153 个源码文件复制到不含模型、target 或 venv 的独立目录；标准库模式下导出器帮助、规则目录校验、参考生成器帮助通过，使用外部测试解释器运行 159 项 oracle 测试通过；此检查不是重新 git clone |
| 独立对照 | 移位后 159 项 Python 单测通过；禁止加载 torch/transformers/tokenizers/numpy/onnx/onnxruntime 后仍全部通过；catalog --check 与已提交 Rust 规则一致 |
| 重新生成预测 | Python 真实 BERT4NER 从同一份 fixture 的 request 独立生成 59 组预测；Rust 比较类型、UTF-8 范围、元数据、分数和脱敏结果全部通过，max score error=2.980232238769531e-7 |
| Rust 与格式 | workspace/all-targets：64 passed、4 ignored；fmt 与包含当时迁移差分 runner 的 Clippy -D warnings 通过 |
| 网关和 TLS | 重新执行真实模型的网关/共享 TLS 集成与 HTTPS CLI 测试通过；本轮未重复原生 OpenSSL hook 全套测试，其此前结果见首节 |
| 安装与运维 | release 构建及 4 项 Node 测试通过，含安装后的网关/TLS 演示；断言许可证和来源声明存在，offline/、tools/、licenses/ 目录与旧 worker 不进入安装包 |
| 无模型 CI | 运维语法检查与 3 项插件边界测试通过；CI 使用 --static，完整安装/真实模型验收仍须单独准备资源后运行 |

当前公开复现入口见 [模型工具](../scripts/model/README.md) 与 [检测器说明](rust-detector.md)。当时的证据位于本地忽略目录 .artifacts/layout-validation/，包含源码副本检查、重新生成的预测、差分结果及回归日志；完整迁移资料、模型、原始权重、运行库、环境与日志均不进入 Git。

## 历史 Python worker 版本验收（feat/censorprivate-pr10）

本节记录保存于 feat/censorprivate-pr10 的旧实现，生产入口当时为 Rust 网关 + 私有本地 Python 检测进程，NER 只有 `shibing624/bert4ner-base-chinese`。以下为本轮实际执行结果；后文旧记录保留其原日期和范围。

| 项目 | 本轮结果 |
| --- | --- |
| 迁移测试 | 159 项 Python 测试通过，含正则、规则、身份证日期/校验位、Luhn、IP、车牌、地址边界、字段别名、NER 偏移和独立评测协议 |
| Rust 回归 | workspace/all-targets 通过；fmt、Clippy `-D warnings` 通过；新增重叠字典回归通过 |
| 故障边界 | worker 超时、退出、无效响应、越界 Unicode、非法分数、重复 JSON 键和超大帧均拒绝处理；缓存不掩盖已退出进程 |
| 真实本地 BERT | regex/rule/NER 同时运行；自然人名、机构名、长文本超过首个窗口、结构化 password 和内嵌工具 JSON 通过 |
| HTTP 与 SSE | 本机 Chat、Messages、Responses 和 SSE 实际转发；捕获的上游请求只有引用，普通隐私恢复、SECRET 遮盖通过 |
| HTTPS CLI | 真实服务进程、本地证书校验、HTTPS 上游、引用计算与 SIGHUP 重载通过；修复引用空白替换导致后续字段名被误识别的问题 |
| TLS | 同一真实 BERT 检测实例完成等长保护和恢复；真实动态 OpenSSL hook、分片、重试、curl 和本机计算测试通过；RPC 从旧 2 秒调整为 60 秒有限等待，凭据复查避免冗余 NER |
| 安装与运维 | release 构建、临时前缀安装及 4 项 Node 验收通过；安装后的启动/停止/重启、认证、网关演示、真实 TLS 演示均通过 |
| 评测复现 | 独立生成的 10 条/27 实体合成样例，在线检测和预测回放指标一致；严格实体 TP/FP/FN=25/3/2，字符覆盖 TP/FP/FN=296/0/2，1 条负例无误报 |

评测中的 NUMBER 金标包含默认 0.6 阈值不保护的泛数字，ID_NUMBER 保守回退及 LOCATION 重叠预测照实计入分类误差。这 10 条合成样例仅验证迁移与评测协议，不代表真实业务精度或完整召回。测试使用本机已有 Python 环境与本地权重，不访问云服务、不自动下载模型。

历史版本的复现命令（须切换到 feat/censorprivate-pr10，不能用于当前源码）：

```bash
export CP_DETECTOR_PYTHON=/path/to/venv/bin/python
export CP_DETECTOR_MODEL_DIR=/path/to/bert4ner-base-chinese
bash scripts/test/detector.sh --integration
CP_TEST_REAL_DETECTOR=1 bash scripts/test/native.sh
bash scripts/test/run-all.sh --detector
```

本节命令和 Python worker 故障测试属于历史版本；当前生产依赖和复现命令见 [检测器说明](rust-detector.md)。

## 迁移前的历史验收

核心复核日期：2026-10-21；DSH 演示与 systemd 静态验证日期：2026-10-16。架构见 [架构说明](architecture.md)。操作方法见 [使用手册](usage.md)。

环境：openEuler / Linux 6.6 x86_64，Rust 1.85.1，Node 22.22.3，OpenSSL 3.0.12，动态 OpenSSL curl 8.4.0；本机已有 DeepSeek Harness 源码，版本 0.1.5-rc.2。

| 项目 | 实际结果 |
| --- | --- |
| 环境检查 | `cpctl doctor --tls --demo --dsh --dsh-root /root/deepseek-harness` 通过 |
| 系统依赖 | dnf 安装命令 dry-run 通过；未实际安装或升级系统包 |
| 安装部署 | release 构建、临时前缀安装、安装后从其他工作目录调用 cpctl 通过；解包产物的 doctor 不调用 Rust 编译器，源码布局仍检查最低版本 |
| 用户实例 | 初始化保留、0600 文件/0700 控制目录、start/check/stop、重启更换 run ID、模型探测通过 |
| 拒绝路径 | 重复启动、运行中更新、失效 PID 记录、非法 origin、错误文件权限、输出符号链接、无效参数拒绝通过 |
| 本机网关 | Rust 网关到模拟上游；上游无虚构原值、SECRET 遮盖、引用求和与最终 300.00 显示通过 |
| 本机 TLS | 真实 OpenSSL hook、Rust broker、自签 CA 校验；等长别名、计算和恢复、非法请求未转发通过 |
| DSH 网关 | 独立 headless profile、真实 agent、模拟模型两次往返、private_compute 和最终显示通过 |
| DSH TLS | 真实 agent 经 tls-run/curl 原 HTTPS 连接，工具别名转 CP 引用、派生结果恢复通过 |
| DSH 日常使用 | 安装产物的 `dsh setup/run` 通过；以 `--help --root` 开头的任务原样到达模拟上游，没有被两层 CLI 解析为选项 |
| 停止与 dry-run | 坏连接配置不阻止按身份停止；依赖预览显示实际包管理命令，systemd 预览的标准输出是独立 unit 正文 |
| 插件边界 | 不支持内容拒绝、计算结果额外原值字段拒绝、非法响应不部分输出、HTTP 总超时、取消等待进程结束通过 |
| 发布归档 | 含 cpctl、五个 Rust 命令、native 库、运维脚本、DSH 插件和独立文档，附 SHA-256 |
| 标准 npm bundle | 发布清单预览通过；未发布、未向已有 DSH profile 安装。实际演示通过绝对模块路径加载同一插件 |
| systemd 静态验证 | 网关与 TLS 两种实际生成 unit 均通过 `systemd-analyze verify`；本轮未安装账户/unit、未启动正式系统服务 |
| 云验收准备与失败路径 | TLS 复用已保存的自定义 native 库路径；故意失败的 DSH stub 验证配置传递、0600 失败报告及凭据不进入报告。此项未调用远端，不计为云效果验收 |
| 真实云服务 | 验收入口与报告已实现；缺有效实际上游/模型/凭据配置，本轮未执行，不计为通过 |
| 官方接入信息核对 | 已联网读取官方 quickstart 与 providers 页面，确认官方模型要求 API key；不带凭据的官方 `GET /models` 实际返回 401。此项仅核对接入要求，不是模型推理验收 |

可重复执行的本机验收：

```bash
bash scripts/test/operations.sh --dsh-root /root/deepseek-harness
```

其中 4 项 Node 测试覆盖插件边界和实际安装/生命周期，随后执行 DSH 的两种完整演示。安装生命周期测试还调用安装产物的网关和 TLS 演示；指定 DSH 路径时加入安装产物的 managed profile 准备及实际 agent 任务，因此同时验证源码和安装后的布局。监听、进程和临时目录仅用于回环测试，测试结束清理。

核心与扩展验收入口：

```bash
bash scripts/test/run-all.sh --native --runtime --operations
```

格式检查、Clippy `-D warnings`、Rust 核心测试（含真实 native、root 运行域及连续三次超时清理）和运维测试通过。207 组固定样例包括 80 组检测与 127 组精确计算，由 Rust 集成测试读取 `tests/fixtures/*.jsonl` 并比较结果，无需外部语言解释器；十进制比较保留精度与字段集合。

补充检查修复 TLS 云验收未沿用自定义 native 库路径的问题；`bash scripts/test/operations.sh` 的 4 项测试再次通过，包含安装后配置传递和云验收失败报告的回归。此回归使用保留测试地址和本机 DSH stub，在发起模型请求前失败，没有实际云调用。

systemd 静态验证分别使用无凭据的回环网关配置、有虚构凭据及测试 CA 的 TLS 配置，核对生成的认证、HTTP/TLS、CA 与库路径参数；没有向正式服务写入配置或启动实例。宿主已有其他 unit 的旧 `/var/run` 路径提示，不影响这两份 unit 验证退出成功。

真实云入口不是固定回复探测：它使用独立虚构策略要求模型发起引用求和，并检查工具回执与授权显示。没有有效配置、响应不受支持或模型没有正确调用工具时失败；网关与 TLS 各自遵守其协议限制。报告不能证明云服务内部的数据存储行为。
