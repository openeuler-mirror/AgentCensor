# CensorPrivacy 文档

先跑通本机检测，再启动网关、接入 Agent。操作文档以当前 Rust + ONNX 实现为准，所有源码命令从 `AgentCensor/CensorPrivate/` 执行。

## 从这里开始

| 你的目标 | 阅读入口 | 完成标志 |
| --- | --- | --- |
| 已拿到完整二进制包，直接运行 | [二进制包](binary-distribution.md) | 解压、校验，`./censorprivacy self-test` 通过 |
| 下载源码，构建并安装 | [构建与部署](build-and-deploy.md) | 模型与运行库就绪，能够打开检测页面 |
| 启动网关、连接实际模型和 Agent | [使用手册](usage.md) | `check --model` 通过，客户端通过网关使用模型 |
| 调整部署参数、检测类别或准备 Kubernetes 挂载 | [统一配置](configuration.md) | config-check 通过，重启后生效 |
| 查某个脚本的参数和用途 | [脚本速查](scripts.md) | 找到入口、前置条件与产物 |
| 启动失败或请求被拒绝 | [故障排查](troubleshooting.md) | 按现象定位配置、资源或协议问题 |

**源码路线：** [下载与构建](build-and-deploy.md#1-下载源码) → [准备模型](build-and-deploy.md#4-准备检测资源) → [打开检测台](usage.md#1-打开隐私检测台) → [启动网关](usage.md#2-启动实际模型网关) → [接入 Agent](usage.md#3-接入-agent)。

**交付包路线：** [获取与校验](binary-distribution.md#1-获取并校验) → [本机自检](binary-distribution.md#2-解压与自检) → [启动网关](usage.md#2-启动实际模型网关)。

## 文档分工

| 文档 | 包含内容 |
| --- | --- |
| [构建与部署](build-and-deploy.md) | 源码下载、系统依赖、Cargo 构建、模型下载/导出、安装、systemd |
| [统一配置](configuration.md) | config.yaml、类别选择、自定义规则、优先级、TTL 与 Kubernetes |
| [使用手册](usage.md) | 检测页面、本机演示、实际网关、DSH/xiaoO、日常管理及可选模式 |
| [二进制包](binary-distribution.md) | 获取归档、校验解压、运行、重新打包与交付验收 |
| [脚本速查](scripts.md) | `censorprivacy`、模型工具、安装打包、各类测试的命令和参数 |
| [故障排查](troubleshooting.md) | 模型/运行库、内存、认证、上游、Agent、systemd、TLS |
| [检测器说明](rust-detector.md) | 资源格式、路径优先级、检测行为、阈值、兼容接口与边界 |
| [架构说明](architecture.md) | 请求链路、授权、组件职责、TLS 与运行域 |
| [验收记录](validation.md) | 按日期保存的实际验证结果和历史记录 |

## 统一约定

| 项目 | 默认值或约定 |
| --- | --- |
| 源码工作目录 | `AgentCensor/CensorPrivate/`，包含 `Cargo.toml` 和 `censorprivacy` |
| 运维入口 | 源码或完整包目录用 `./censorprivacy`；源码安装后可用 `~/.local/bin/censorprivacy` |
| 私有配置目录 | `~/.config/censorprivacy/managed/`；多实例统一传入 `--config-dir` |
| 用户安装前缀 | `~/.local`；系统服务通常安装到 `/usr/local` |
| 网关端口 | `censorprivacy` 默认 `8775`；直接运行 `censorprivacy serve` 默认 `8765` |
| 检测页面端口 | `8766`，仅本机访问 |
| DSH Web 端口 | `3080`，与检测页面是两个独立入口 |
| 示例路径 | `/path/to/...` 必须换成实际路径；依赖绝对路径的参数按示例填写 |

`bert4ner-base-chinese` 是本地隐私检测模型；`init --model-id` 指上游聊天模型。源码 Git 仓库不包含模型权重、ONNX Runtime、凭据或完整 Python 迁移资料。生产运行无需 Python；只有首次从原始权重导出 ONNX 时需要模型工具环境。
