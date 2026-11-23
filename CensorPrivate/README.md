# CensorPrivacy

**在本机识别隐私，保护发给模型的内容。**

CensorPrivacy 是 AgentCensor 的模型数据隐私组件。它在本地检测敏感信息，将原文替换为随机引用后发送给模型，再在授权的回复显示位置恢复；密码、密钥等 SECRET 始终遮盖。核心与服务使用 Rust，NER 使用本地 ONNX，生产运行无需 Python。

当前提供 16 个内置检测类别：`ADDRESS` 只保护通讯地址，`IP_ADDRESS` 独立保护 IP，已移除 `MEDICAL_INFO`。`PERSON / ORG / LOCATION / DATE` 全部关闭时自动跳过 NER 模型加载与推理；只保留 DATE 仍会运行 NER。配置与规则模式示例见 [类别选择](docs/configuration.md#类别选择)。

产品名统一为 **CensorPrivacy**，命令与发行包使用 **`censorprivacy`**。网关、运维、私密计算、MCP 和客户端隔离功能均通过同一个命令的子命令使用；源码目录仍为 `AgentCensor/CensorPrivate/`。

[使用完整包](#使用完整包) · [开始使用](#开始使用) · [Agent 接入](#agent-接入) · [日常管理](#日常管理) · [从源码构建](#从源码构建) · [完整文档](docs/README.md)

## 使用完整包

推荐使用 **standalone 完整包**，内置可执行程序、ONNX 模型、ONNX Runtime、Node 和运维依赖，解压后即可使用。已验收平台为 **openEuler 24.03 LTS / x86_64 / glibc 2.38+**；DSH 和 xiaoO 需另行安装。

从发布方取得以下同版本归档与 `.sha256` 文件，在文件所在目录执行：

```bash
sha256sum -c censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz.sha256
tar -xzf censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz
cd censorprivacy-0.3.2-linux-x86_64-standalone
```

后续命令均在解压后的包目录执行。获取方式、包内自检与版本兼容性见 [二进制包说明](docs/binary-distribution.md)。需要自行编译时，转到 [从源码构建](#从源码构建)。

## 开始使用

### 1. 初始化模型连接

以下以 DeepSeek 为例，其他服务替换为实际 origin 与模型 ID：

```bash
./censorprivacy init --upstream https://api.deepseek.com --model-id deepseek-flash
```

按提示输入 provider API Key，输入不回显。上游地址不加 `/v1`；`--model-id` 指上游聊天模型，本地隐私检测使用 `bert4ner-base-chinese`。已有实例再次执行 `init` 会保留配置和凭据。

### 2. 调整配置

按使用方式找到配置文件：

| 使用方式 | 配置文件 |
| --- | --- |
| 完整包 | `share/censorprivacy/config.yaml` |
| 源码目录 | `config.yaml` |
| 默认用户安装 | `~/.local/share/censorprivacy/config.yaml` |

在 `custom` 中补充业务词典、正则和秘密；`detection.categories` 选择检测类别，随附配置为 `all`，省略、`null` 或 `[]` 表示关闭按类别检测。`ner` 管理线程、设备、batch 与每窗口超时，`server.ttl_seconds` 设置运行有效期。字段说明、优先级与 Kubernetes 挂载见 [统一配置](docs/configuration.md)。

### 3. 启动网关

```bash
./censorprivacy start
./censorprivacy check
```

启动日志会输出网关地址，默认 [http://127.0.0.1:8775/](http://127.0.0.1:8775/)。直接点击或复制到浏览器，即可查看隐私检测与 Agent 会话，页面共用网关已加载的模型。远程服务器通过 [SSH 端口转发](docs/usage.md#23-启动并检查) 访问。

`check` 检查本机版本、权限、认证和 run 状态。需要验证实际模型连通性时执行 `./censorprivacy check --model`，该命令会调用上游并可能计费。

## Agent 接入

保持网关运行，选择已有客户端或项目适配入口。

### DeepSeek Harness

兼容性验证基线为 **DSH `0.2.2-rc.1`**（源码标签 `dsh-v0.2.2-rc.1`）。按 [该版本官方说明](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.1/README.zh.md#run-from-source) 安装或构建，再启动聊天页面；`censorprivacy` 不限制 DSH 版本。该版需要 Node >=24.9.0，支持官方 npm 包和源码构建。准备步骤见 [使用手册](docs/usage.md#32-deepseek-harness浏览器聊天)。

```bash
./censorprivacy dsh web --dsh-path /path/to/deepseek-harness
```

打开终端打印的**带 token 的完整网址**，默认端口为 `3080`，选择工作目录后即可聊天。默认同时开放 DSH 自带工具和私密工具；添加 `--disable_tool` 可关闭 DSH 自带工具，仅保留私密工具。

顶栏“轨迹”之后的“隐私”可查看当前会话的系统提示、用户输入和工具上下文，逐次对照原文与实际发往模型的遮蔽文本。支持自动同步、历史请求切换和窄屏显示；使用 gateway 模式。独立插件构建与打包见 [插件说明](plugins/dsh-censorprivacy/README.md)。

### openEuler xiaoO

准备已构建的 xiaoO 后，启动终端交互：

```bash
./censorprivacy xiaoo tui --xiaoO-path /path/to/xiaoO
```

xiaoO 默认关闭自身工具。需要文件、Shell 等工具时加 `--enable-tools`；启动器会先加载工具目录并预热本地检测，准备请求不调用云模型，也不保存为会话。

两者自动复用当前网关的模型与凭据，使用独立配置，不覆盖原客户端配置。PATH 已有可用命令时可省略源码目录。可粘贴 [虚构费用工单](examples/demo.txt) 试用；完整准备和工具调用说明见 [Agent 接入指南](docs/usage.md#3-接入-agent)。

会话会自动同步到网关页面，点击消息可查看原文、检测结果和实际遮蔽文本。记录保存在本机私有配置目录的 `xiaoo/records/` 中，刷新和重启后可回看；记录管理见 [使用手册](docs/usage.md#35-通用-agent-会话检测台)。

### 通用客户端

| 配置项 | 填写内容 |
| --- | --- |
| Chat Completions Base URL | `http://127.0.0.1:8775/v1` |
| API Key | `~/.config/censorprivacy/managed/gateway.token` 的内容 |
| 模型 ID | 与 `init --model-id` 一致 |

Agent 使用本机 gateway token，provider Key 由网关持有。Responses/Messages 路径及其他 Agent 的会话同步方式见 [使用手册](docs/usage.md#31-通用客户端)。

## 日常管理

| 命令 | 用途 |
| --- | --- |
| `./censorprivacy check` | 检查当前网关认证、权限与 run 状态，无上游调用 |
| `./censorprivacy dashboard` | 查看当前网关的页面地址 |
| `./censorprivacy stop` | 停止当前用户实例 |
| `./censorprivacy start` | 启动新 run；Agent 需新建会话 |
| `./censorprivacy demo gateway` | 真实检测器 + 模拟上游，演示遮蔽、恢复与引用计算，无需云 Key |
| `./censorprivacy help` | 查看命令与选项 |

映射只保存在服务内存中，重启后旧引用失效，Agent 需重新启动并新建会话。随附配置的有效期为 24 小时，由 `server.ttl_seconds` 设置。多实例、systemd 与更新流程见 [日常管理](docs/usage.md#4-日常管理)。

## 从源码构建

源码构建需要 Linux、[Rust ≥1.85](https://www.rust-lang.org/tools/install) 和 C 链接器。先安装 Rust 工具链，再下载并编译：

```bash
git clone https://gitcode.com/coco12373/AgentCensor.git
cd AgentCensor/CensorPrivate
./censorprivacy deps
cargo build --locked --release --bins
```

`deps` 按系统选择 apt/dnf 安装依赖，可能使用 sudo；`--dry-run` 只预览。源码运行本机演示另需 Node 22.19+（22.x）或 ≥24；DSH 0.2.2-rc.1 需要 Node ≥24.9。

**编译完成后还需准备模型与 ONNX Runtime。** 按 [模型下载与 ONNX 导出步骤](docs/build-and-deploy.md#4-准备检测资源) 准备资源，在根目录 `config.yaml` 的 `ner.model_dir`、`ner.ort_library` 中填写实际路径，然后校验：

```bash
./target/release/censorprivacy config-check --config ./config.yaml
```

`config-check` 检查配置字段，模型和运行库在启动时验证。已有可信 ONNX 包可直接使用；只有从原始权重导出 ONNX 时需要 Python。

### 安装到当前用户目录

```bash
./censorprivacy install
```

`install` 会构建并安装 Rust 命令，复用已有编译缓存，默认安装到 `~/.local`。安装后可在任意目录使用 `~/.local/bin/censorprivacy`，配置文件为 `~/.local/share/censorprivacy/config.yaml`。首次安装不会沿用源码配置中的模型和运行库路径，需在安装后的配置中填写 `ner.model_dir`、`ner.ort_library`，模型与运行库本身需另行准备。

随后按 [开始使用](#开始使用) 初始化并启动网关，将命令中的 `./censorprivacy` 换为 `~/.local/bin/censorprivacy`。只在源码目录运行时，可省略安装，直接使用 `./censorprivacy` 调用 `target/release/` 中的构建产物。公共安装、systemd 与完整依赖见 [构建与部署](docs/build-and-deploy.md)。

## 工作方式与可选能力

**Agent 请求 → 本机检测 → 隐私替换 → 模型服务 → 回复检查 → 授权显示恢复。**

正则、规则、验证与 NER 在同一 Rust 进程中完成，原值保留在本机 vault。普通网关支持已实现的 Chat Completions、Responses、Messages 及文本 SSE；代码、链接目标与普通工具参数不恢复原值。未知格式或检测失败时拒绝处理。检测范围、阈值和协议边界见 [检测器说明](docs/rust-detector.md) 与 [架构说明](docs/architecture.md)。

按场景启用额外能力：

- [私密计算与 MCP](docs/usage.md#私密计算与-mcp)：用授权引用完成本地数值/日期运算，返回派生引用。
- [Linux 受管运行域](docs/usage.md#linux-受管运行域)：通过 `censorprivacy run` 约束受管进程及后代的网络出口，需要 root 与相应内核能力。
- [TLS direct](docs/usage.md#tls-direct)：通过 `censorprivacy tls-run` 与可选 native hook 保留受支持客户端的原 TLS 连接。普通网关和检测台无需构建 native。

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
| [统一配置](docs/configuration.md) | 部署参数、类别、自定义规则、TTL 与 Kubernetes 挂载 |
| [构建与部署](docs/build-and-deploy.md) | 源码下载、依赖、模型导出、安装与 systemd |
| [使用手册](docs/usage.md) | 网关启动、Agent 接入、会话查看及可选模式 |
| [二进制包](docs/binary-distribution.md) | 获取、校验、解压、自检与打包 |
| [脚本速查](docs/scripts.md) | 运维、模型工具及测试参数 |
| [故障排查](docs/troubleshooting.md) | 资源、内存、认证、上游与客户端问题 |

## 许可证

沿用 AgentCensor 的 [木兰宽松许可证，第 2 版](../LICENSE)。第三方代码与模型来源见 [许可说明](licenses/README.md)。
