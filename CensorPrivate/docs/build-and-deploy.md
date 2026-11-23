# 构建与部署

[文档首页](README.md) · [使用手册](usage.md) · [脚本速查](scripts.md) · [故障排查](troubleshooting.md)

从源码完成 **下载 → 构建 → 准备检测资源 → 验证 → 安装**。日常启动与 Agent 接入统一使用 `cpctl`，不再另外创建一套配置目录。

已有完整二进制包时，直接按 [二进制包说明](binary-distribution.md) 操作，无需执行本页的编译与模型导出。

- [1. 下载源码](#1-下载源码)
- [2. 准备构建环境](#2-准备构建环境)
- [3. 编译 Rust 命令](#3-编译-rust-命令)
- [4. 准备检测资源](#4-准备检测资源)
- [5. 验证并安装](#5-验证并安装)
- [6. 部署为系统服务](#6-部署为系统服务)
- [7. 构建与部署参考](#7-构建与部署参考)

## 1. 下载源码

准备 Linux、Git 和可访问依赖源的网络，然后执行：

```bash
git clone https://gitcode.com/coco12373/AgentCensor.git
cd AgentCensor/CensorPrivate
```

后续命令均在这个目录运行。更新已有 checkout 时先保存本地修改，再获取并检出要部署的版本；正式交付记录具体提交号：

```bash
git rev-parse HEAD
```

## 2. 准备构建环境

先安装 [Rust 工具链](https://www.rust-lang.org/tools/install)，版本至少为 **1.85**，并包含 `rustfmt`、`clippy`。按系统选择基础包：

```bash
# openEuler / RHEL 系
sudo dnf install gcc make pkgconf-pkg-config ca-certificates curl jq openssl util-linux
```

```bash
# Ubuntu / Debian
sudo apt-get update
sudo apt-get install build-essential pkg-config ca-certificates curl jq openssl util-linux
```

也可以让仓库脚本选择包管理器，先预览再执行：

```bash
./cpctl deps --dry-run
./cpctl deps
rustc --version
cargo --version
```

`deps` 安装系统包，不安装 Rust 工具链或模型。普通网关使用 Rustls，无需 OpenSSL 开发库；`openssl` 命令用于生成本机令牌。运行 `demo` 或 DSH 另需 Node **22.19+（22.x）或 ≥24**；见后文 [依赖表](#用途与依赖)。

内存较小时，在当前终端限制构建和测试并发，模型导出、编译、推理及浏览器验收依次执行：

```bash
export CARGO_BUILD_JOBS=1
export RUST_TEST_THREADS=1
```

## 3. 编译 Rust 命令

```bash
cargo build --locked --release --bins
./target/release/censorprivacy --version
```

产物位于 `target/release/`：`censorprivacy`、`privacy-run`、`privcalc`、`censorprivacy-mcp`、`tls-run`。首次构建会获取 Cargo 依赖，`--locked` 使用仓库锁文件。

**编译成功后仍需准备模型与 ONNX Runtime，才能启动检测页面或网关。** Cargo 不下载这两项资源。默认 Rust 回归不需要模型：

```bash
bash scripts/test/run-all.sh
```

## 4. 准备检测资源

已有可信、完整的 ONNX 资源包时，直接进入 [配置资源路径](#43-配置资源路径)。从 Hugging Face 下载的原始权重需要先导出，不能直接作为产品的 ONNX 包使用。

### 4.1 下载原始模型

首次导出需要 Python **≥3.11**。在本项目中建立独立准备环境：

```bash
python3 -m venv .venv-model
.venv-model/bin/python -m pip install 'torch==2.14.1' --index-url https://download.pytorch.org/whl/cpu
.venv-model/bin/python -m pip install -r scripts/model/requirements.txt
.venv-model/bin/hf download shibing624/bert4ner-base-chinese \
  --revision 5d660ed2aa9da482bf2d99c6bc8cf2ce66758f6a \
  --local-dir .artifacts/model-source/bert4ner-base-chinese
```

源文件至少包括 `config.json`、`tokenizer_config.json`、`special_tokens_map.json`、`vocab.txt` 和 `model.safetensors`。已有相同权重时，可直接使用本地目录，省略下载。

### 4.2 导出并校验 ONNX

```bash
export CP_SOURCE_MODEL_DIR="$PWD/.artifacts/model-source/bert4ner-base-chinese"
.venv-model/bin/python -B scripts/model/export.py \
  --source "$CP_SOURCE_MODEL_DIR" --check-source
HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
  .venv-model/bin/python -B scripts/model/export.py \
  --source "$CP_SOURCE_MODEL_DIR" --output models/bert4ner-base-chinese
```

`--check-source` 核对已验收的源文件哈希；导出命令生成 FP32 ONNX，并自动比较 PyTorch 与 ONNX 的 token 标签及数值误差。成功后，输出目录应包含：

```text
models/bert4ner-base-chinese/
├── model.onnx
├── tokenizer.json
├── config.json
├── manifest.json
└── export-validation.json
```

完整依赖、导出参数及数值标准见 [模型工具说明](../scripts/model/README.md)。准备环境只用于生成资源，Rust 服务不启动 Python。

### 4.3 配置资源路径

模型导出依赖中已经包含 ONNX Runtime wheel，可以使用其中的原生库：

```bash
export CP_DETECTOR_MODEL_DIR="$PWD/models/bert4ner-base-chinese"
export CP_DETECTOR_ORT_LIBRARY="$(find "$PWD/.venv-model/lib" \
  -path '*/onnxruntime/capi/libonnxruntime.so*' -type f -print -quit)"
test -f "$CP_DETECTOR_ORT_LIBRARY"
./cpctl doctor
```

已有官方 CPU 运行库时，把 `CP_DETECTOR_ORT_LIBRARY` 改为实际的 `.so` 路径；版本文件名如 `libonnxruntime.so.1.31.1` 也可使用。本机已验收版本为 **ONNX Runtime 1.31.1 CPU / Linux x86_64**，其他版本或平台需单独验收。

开发机可以直接引用准备环境内的原生库。正式服务应将模型和运行库放入稳定目录，例如 `/opt/censorprivacy-models/` 与 `/opt/onnxruntime/lib/`，不要依赖会被删除的虚拟环境。迁移运行库时保留发行包要求的相邻库文件及许可证。

这两个变量需要在**每个启动终端**中设置；`cpctl init` 不保存它们。也可以采用 [默认安装布局](rust-detector.md#2-配置路径)。系统服务的持久配置见 [第 6 节](#6-部署为系统服务)。

## 5. 验证并安装

### 5.1 先打开本地检测页面

保持前文资源变量有效，运行：

```bash
./target/release/censorprivacy web
```

模型校验与预热成功后，终端打印 `http://127.0.0.1:8766/`。页面可查看原文标注、来源和遮蔽预览，无需上游模型或 API Key。按 Ctrl+C 停止；服务器远程访问方法见 [使用手册](usage.md#1-打开隐私检测台)。

需要验证真实 ONNX 与网关行为时，在页面服务停止后执行：

```bash
bash scripts/test/detector.sh --integration
```

### 5.2 安装到当前用户目录

```bash
./cpctl install
"$HOME/.local/bin/censorprivacy" --version
"$HOME/.local/bin/cpctl" doctor
```

默认前缀为 `~/.local`。`install` 会按锁文件构建 release，复制命令、运维脚本、插件和文档；**不复制模型或 ONNX Runtime**。安装后的命令仍使用前文资源变量。

需要公共安装或系统服务时改用：

```bash
./cpctl install --prefix /usr/local
```

构建由当前账户执行，复制到受保护目录时使用 sudo。接下来转到 [使用手册：启动实际模型网关](usage.md#2-启动实际模型网关)，完成 `init → start → check --model`。需要分发归档时，参阅 [打包说明](binary-distribution.md#4-从源码打包)。

## 6. 部署为系统服务

先完成 `/usr/local` 安装、`cpctl init` 及实际模型连通性检查。将检测资源放在服务账户可读的公共目录，并在执行 `systemd install` 的终端中设置实际路径：

```bash
export CP_DETECTOR_MODEL_DIR=/opt/censorprivacy-models/bert4ner-base-chinese
export CP_DETECTOR_ORT_LIBRARY=/opt/onnxruntime/lib/libonnxruntime.so
./cpctl systemd install --dry-run
./cpctl systemd install
./cpctl systemd start
./cpctl systemd check
```

`systemd install` 创建专用 `censorprivacy` 账户，将配置、策略与凭据复制到 `/etc/censorprivacy/`，并把检测资源变量写入实例环境文件。模板启用 `ProtectHome=yes`，模型、运行库和可执行文件不要放在 `/root` 或用户 home 下。

`systemd start` 会停止同配置的用户实例，启动后自动检查；`systemd check` 同步当前 run 文件，供 Agent 接入使用。生成的服务名包含用户 ID 与配置目录散列，命令会打印实际名称；不会自动启用开机启动。

```bash
./cpctl systemd stop
```

修改策略、连接配置、凭据或资源路径后，依次执行 `systemd stop → systemd install → systemd start`。重启会清空原值映射，客户端须新建会话。手工模板与目录权限见 [deploy/README.md](../deploy/README.md)。

## 7. 构建与部署参考

### 用途与依赖

| 用途 | 额外条件 |
| --- | --- |
| 编译 Rust 产品 | Rust ≥1.85、C 链接器、锁文件中的 Cargo 依赖 |
| 网关与检测页面 | 本地 FP32 ONNX 包、匹配平台的 ONNX Runtime 原生库 |
| ONNX 首次导出 | Python ≥3.11、CPU PyTorch 及 `scripts/model/requirements.txt` |
| `cpctl` 运维 | Linux、Bash、curl、jq、openssl、GNU coreutils、flock |
| 本机演示与 DSH | Node 22.19+（22.x）或 ≥24；DSH 另需可用的 Harness |
| TLS direct | C11、make、OpenSSL 开发包、动态 OpenSSL 客户端、native hook |
| Linux 受管运行 | root、iproute2、nftables、unshare、libseccomp、相应内核能力 |

### 可选构建

普通网关与检测页面无需 `native/`。保留客户端原 TLS 连接的 TLS direct 模式需要：

```bash
./cpctl deps --tls --dry-run
./cpctl deps --tls
make -C native
./cpctl install --tls
```

产物为 `native/libcp_tls_guard.so`。`cpctl install --tls` 等价于在安装时启用 `scripts/install.sh --with-native`，见 [native 说明](../native/README.md)。

### 更新与回退

保存旧归档、SHA-256 和匹配配置，在独立目录构建并验证新版本，停止目标实例后再替换安装文件。回退同样执行“停止 → 恢复旧产物与配置 → 启动 → 检查”。新旧版本均不能恢复上一次进程的内存 vault，Agent 必须新建会话。

### 脚本与问题入口

安装参数、暂存安装、两种打包方式及测试选项统一查阅 [脚本速查](scripts.md)。缺少模型、运行库、编译 OOM 或 systemd 权限问题，查阅 [故障排查](troubleshooting.md)。
