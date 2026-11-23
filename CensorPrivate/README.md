# CensorPrivate

**在本机识别隐私，保护发给模型的内容。**

CensorPrivate 是 AgentCensor 的模型数据隐私组件。它在本地检测敏感信息，将原文替换为随机引用后发送给模型，再在授权的回复显示位置恢复；密码、密钥等 SECRET 始终遮盖。核心与服务使用 Rust，NER 使用本地 ONNX，生产运行无需 Python。

[快速开始](#快速开始) · [隐私检测台](#隐私检测台) · [模型网关](#模型网关) · [Agent 接入](#agent-接入) · [完整文档](docs/README.md)

## 快速开始

| 使用方式 | 准备内容 |
| --- | --- |
| [源码构建](#从源码构建) | Linux、Rust ≥1.85、C 链接器、模型与运行库，适合开发和自行部署 |
| [完整包](#使用完整包) | standalone 归档与 SHA-256，包含模型和运行库，适合在已验收平台直接运行 |

### 从源码构建

**1. 下载并编译**

```bash
git clone https://gitcode.com/coco12373/AgentCensor.git
cd AgentCensor/CensorPrivate
./cpctl deps --dry-run
./cpctl deps
cargo build --locked --release --bins
```

先安装 [Rust 工具链](https://www.rust-lang.org/tools/install)。`deps` 按系统选择 apt/dnf，安装系统包时可能使用 sudo；`--dry-run` 只预览。后续源码命令均在 `CensorPrivate/` 目录执行。

**2. 准备检测模型与运行库**

按 [模型下载与 ONNX 导出步骤](docs/build-and-deploy.md#4-准备检测资源) 准备资源，已有可信 ONNX 包时可以直接使用。将下面的路径替换为实际位置：

```bash
export CP_DETECTOR_MODEL_DIR="$PWD/models/bert4ner-base-chinese"
export CP_DETECTOR_ORT_LIBRARY=/path/to/libonnxruntime.so
./cpctl doctor
```

**Cargo 编译不下载或生成模型、ONNX Runtime。** 从原始权重导出 ONNX 才需要 Python；运行 Rust 服务只需导出的模型包与原生库。资源路径需在每个启动终端配置，也可采用 [默认安装布局](docs/rust-detector.md#2-配置路径)。

**3. 开始使用**

先运行下节的检测台，无需云账号；需要模型交互时再启动网关。可选安装到当前用户目录：

```bash
./cpctl install
```

安装后可在任意目录运行 `~/.local/bin/cpctl`。安装脚本复制命令、插件和文档，模型与 ONNX Runtime 单独准备。系统服务、公共安装和完整依赖见 [构建与部署](docs/build-and-deploy.md)。

### 使用完整包

从发布方取得匹配版本的归档与 `.sha256` 文件，在两者所在目录执行：

```bash
sha256sum -c censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz.sha256
tar -xzf censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz
cd censorprivacy-0.3.2-linux-x86_64-standalone
./cpctl self-test
```

完整包已验收平台为 **openEuler 24.03 LTS / x86_64 / glibc 2.38+**。自检使用真实本地检测器和模拟上游，无云调用；结束后自动停止临时进程。`self-test` 仅完整包提供，下载、版本与重新打包见 [二进制包说明](docs/binary-distribution.md)。

## 隐私检测台

资源就绪后启动：

```bash
# 源码构建产物
./target/release/censorprivacy web
```

完整包使用 `./bin/censorprivacy web`；源码安装后使用 `~/.local/bin/censorprivacy web`。

打开 **http://127.0.0.1:8766/**，粘贴文本并检测，即可查看：

- **原文高亮**：定位命中的隐私范围。
- **检测依据**：展示 REGEX / RULES / NER 来源、分数与校验状态。
- **遮蔽预览**：切换随机引用、类别占位符或等字符数星号，调整遮蔽阈值。

检测台不需要上游或 API Key，预览引用不能用于网关恢复。Ctrl+C 停止；内存较小时，先停止检测台再启动网关。服务器访问使用 [SSH 端口转发](docs/usage.md#1-打开隐私检测台)，详细交互见 [检测台说明](plugins/detector-web/README.md)。

## 模型网关

准备好本地检测资源后，按 **初始化 → 配置策略 → 启动 → 检查** 操作。以下以 DeepSeek 为例，其他服务替换为实际 origin 与模型 ID：

```bash
./cpctl init --upstream https://api.deepseek.com --model-id deepseek-flash
```

按提示输入 provider API Key，输入不回显。上游地址不加 `/v1`；`--model-id` 指上游聊天模型，本地隐私检测模型固定为 `bert4ner-base-chinese`。

初始化后编辑 `~/.config/censorprivacy/managed/policy.yaml`，补充需要保护的业务词典、正则和秘密，再执行：

```bash
./cpctl start
./cpctl check --model
```

网关默认地址为 `http://127.0.0.1:8775`。`check --model` 会调用实际上游，可能计费。私有凭据、策略示例、多实例与 systemd 使用见 [使用手册](docs/usage.md)。

## Agent 接入

保持网关运行，选择已有客户端或项目适配入口。

### 通用客户端

| 配置项 | 填写内容 |
| --- | --- |
| Chat Completions Base URL | `http://127.0.0.1:8775/v1` |
| API Key | `~/.config/censorprivacy/managed/gateway.token` 的内容 |
| 模型 ID | 与 `init --model-id` 一致 |

Agent 使用本机 gateway token，provider Key 由网关持有。Responses/Messages 等接口的路径配置见 [使用手册](docs/usage.md#31-通用客户端)。

### DeepSeek Harness 与 openEuler xiaoO

先按各自官方说明安装或构建客户端，再执行对应入口：

```bash
# DeepSeek Harness：浏览器聊天
./cpctl dsh web --dsh-path /path/to/deepseek-harness
```

```bash
# openEuler xiaoO：终端交互
./cpctl xiaoo tui --xiaoO-path /path/to/xiaoO
# 查看会话检测台地址（默认 http://127.0.0.1:8775/）
./cpctl xiaoo web
```

两者自动复用当前网关的模型与凭据，使用独立配置，不覆盖原客户端配置。PATH 已有可用命令时可省略源码目录。完整准备、聊天示例和工具调用验收见 [Agent 接入指南](docs/usage.md#3-接入-agent)。

xiaoO 默认关闭自身工具。需要文件、Shell 等工具时加 `--enable-tools`，启动器会在进入对话前加载真实工具目录并预热本地隐私检测，显示准备状态；准备请求不调用云模型，也不保存为会话。NER 线程数可通过 `init --ner-threads N` 持久保存，或 `start --ner-threads N` 临时覆盖，默认 1。 NER 支持 `start --ner-device cpu --ner-threads 4` 和 `start --ner-device cuda --ner-batch-size 4`，多卡使用 `--ner-device cuda:0,1,2,3`；昇腾使用 `--ner-device cann` 或 `cann:0,1`。CUDA/CANN 须另配对应原生运行库及设备依赖，配置见 [检测器说明](docs/rust-detector.md#2-配置路径)。

DeepSeek Harness CLI/Web 和 xiaoO 会话自动同步到隐私检测台，运行 `./cpctl web` 查看地址；页面标明 Agent 来源，点击消息可查看原文、检测结果和实际遮蔽文本。对话与工具结果保存在本机私有目录 `xiaoo/records/`，刷新和重启后可回看；网页与网关共用模型。更新后重启网关与 Agent 客户端。其他 OpenAI Chat Completions Agent 可通过 Base URL 或请求头指定来源与会话，见 [通用接入说明](plugins/detector-web/README.md)。

## 日常管理与本机演示

| 命令 | 用途 |
| --- | --- |
| `./cpctl check` | 检查当前网关认证、权限与 run 状态，无上游调用 |
| `./cpctl stop` | 停止当前用户实例 |
| `./cpctl start` | 启动新 run；Agent 需新建会话 |
| `./cpctl demo gateway` | 真实检测器 + 模拟上游，演示遮蔽、恢复与引用计算，无需云 Key |
| `./cpctl help` | 查看命令与选项 |

演示另需 Node 22.19+（22.x）或 ≥24。映射只保存在服务内存中，重启后旧引用失效；默认 run 有效期为一小时，调整与排查见 [日常管理](docs/usage.md#4-日常管理)。

## 工作方式与可选能力

**Agent 请求 → 本机检测 → 隐私替换 → 模型服务 → 回复检查 → 授权显示恢复。**

正则、规则、验证与 NER 在同一 Rust 进程中完成，原值保留在本机 vault。普通网关支持已实现的 Chat Completions、Responses、Messages 及文本 SSE；代码、链接目标与普通工具参数不恢复原值。未知格式或检测失败时拒绝处理。检测范围、阈值和协议边界见 [检测器说明](docs/rust-detector.md) 与 [架构说明](docs/architecture.md)。

按场景启用额外能力：

- [私密计算与 MCP](docs/usage.md#私密计算与-mcp)：用授权引用完成本地数值/日期运算，返回派生引用。
- [Linux 受管运行域](docs/usage.md#linux-受管运行域)：通过 `privacy-run` 约束受管进程及后代的网络出口，需要 root 与相应内核能力。
- [TLS direct](docs/usage.md#tls-direct)：通过 `tls-run` 与可选 native hook 保留受支持客户端的原 TLS 连接。普通网关和检测台无需构建 native。

## 开发与交付

```bash
# 默认回归，无需模型或 Python
bash scripts/test/run-all.sh
```

修改检测器时，另行配置 ONNX/ORT 后执行 `bash scripts/test/detector.sh --integration`。安装、打包、浏览器及环境相关测试的条件与参数见 [脚本速查](docs/scripts.md)；已执行的测试范围见 [验收记录](docs/validation.md)。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [文档首页](docs/README.md) | 阅读路线与统一约定 |
| [构建与部署](docs/build-and-deploy.md) | 源码下载、依赖、模型导出、安装与 systemd |
| [使用手册](docs/usage.md) | 检测台、网关、Agent 接入及可选模式 |
| [二进制包](docs/binary-distribution.md) | 获取、校验、解压、自检与打包 |
| [脚本速查](docs/scripts.md) | 运维、模型工具及测试参数 |
| [故障排查](docs/troubleshooting.md) | 资源、内存、认证、上游与客户端问题 |

## 许可证

沿用 AgentCensor 的 [木兰宽松许可证，第 2 版](../LICENSE)。第三方代码与模型来源见 [许可说明](licenses/README.md)。
