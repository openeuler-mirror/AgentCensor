# 脚本速查

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [使用手册](usage.md) · [故障排查](troubleshooting.md)

日常操作使用 `cpctl`，开发与交付使用 `scripts/`。下文路径均相对于 `CensorPrivate/`；运行安装、打包或模型验收时，等待当前任务结束再开始下一项。

## 1. 先选入口

| 需要做什么 | 入口 | 产物或结果 |
| --- | --- | --- |
| 下载源码 | `git clone https://gitcode.com/coco12373/AgentCensor.git` | AgentCensor checkout，进入 `CensorPrivate/` |
| 下载原始模型 | `.venv-model/bin/hf download …` | 原始权重目录，完整命令见 [构建步骤](build-and-deploy.md#41-下载原始模型) |
| 下载完整交付包 | 从发布方提供的地址取得归档与校验文件 | 见 [二进制包](binary-distribution.md#1-获取并校验)；仓库没有自动下载脚本 |
| 检查/准备系统环境 | `./cpctl doctor`、`./cpctl deps` | 检查结果或系统软件包 |
| 构建并安装 | `./cpctl install` | 五个命令、`cpctl`、插件、文档 |
| 导出 ONNX | `scripts/model/export.py` | 模型、tokenizer、manifest、数值报告 |
| 生成归档 | `bash scripts/package.sh` | `dist/` 下的归档与 SHA-256 |
| 启停与接入 Agent | `./cpctl …` | 私有配置、实例进程、Agent profile |
| 回归与验收 | `scripts/test/` | 测试结果；具体资源要求见 [第 4 节](#4-测试与验收) |

## 2. cpctl 命令

```bash
./cpctl help
```

源码入口将操作分派给 `scripts/ops/`，不要直接运行其中的 `main.sh`、`common.sh`、`deploy.sh`、`environment.sh` 或 `xiaoo.sh`。它们依赖入口设置的上下文。

### 环境与安装

| 命令 | 常用选项 | 行为 |
| --- | --- | --- |
| `doctor` | `--tls --demo --dsh --runtime`、`--dsh-root DIR` | 只读检查；检测资源未准备时会失败 |
| `deps` | `--tls --demo --dsh --runtime --dry-run` | 选择 apt/dnf；需要时 sudo；不安装 Rust、模型或 DSH |
| `install` | `--prefix DIR --tls --dry-run` | 从源码构建安装；默认 `~/.local`；`--tls` 加入 native |

### 配置与生命周期

| 命令 | 常用选项 | 行为 |
| --- | --- | --- |
| `init` | `--mode gateway\|tls --upstream ORIGIN --model-id ID --port PORT` | 首次准备配置；已有配置保留；默认 gateway/8775 |
| `init` 凭据参数 | `--key-file FILE` 或 `--no-key`；`--ca-file FILE` | 读取私有 Key 或指定无认证上游；可复制私有 CA |
| `init` TLS 参数 | `--native-library FILE` | 保存 TLS direct 使用的 hook 路径 |
| `start` / `stop` | 通用实例选项 | 启停当前用户实例，停止时核对进程身份 |
| `check` | `--model` | 默认只检查本机；`--model` 实际调用上游，可能计费 |
| `systemd install` | `--dry-run` | 生成实例 unit，复制当前配置；需要公共安装 |
| `systemd start` / `check` / `stop` | 通用实例选项 | 管理对应系统实例；具体服务名由脚本生成 |

通用选项 `--config-dir /绝对路径` 选择实例，默认 `~/.config/censorprivacy/managed/`。`--prefix /绝对路径` 设置安装前缀；它不改变配置目录。`init` 不保存检测模型与运行库的环境变量，见 [路径说明](rust-detector.md#2-配置路径)。

### 演示与 Agent

| 命令 | 参数 | 上游 |
| --- | --- | --- |
| `demo gateway` / `demo tls` | TLS 可用 `--native-library FILE` | 自动启动本机模拟上游，无需云 Key |
| `demo dsh-gateway` / `demo dsh-tls` | `--dsh-root DIR` | 真实 Agent + 本机模拟上游 |
| `dsh web` | `--dsh-root DIR --port 3080` | 当前配置的实际上游 |
| `dsh setup` / `run` / `cloud` | `--dsh-root DIR`；`run` 接任务文本 | `run/cloud` 使用实际上游，可能计费 |
| `xiaoo setup` / `tui` / `run` / `demo` | `--xiaoo-root DIR` 或 `--xiaoo-bin FILE` | `tui/run/demo` 使用实际上游，可能计费 |

任务含以 `-` 开头的内容时，用 `--` 分隔；配置选项放在分隔符之前：

```bash
./cpctl dsh run --config-dir "$HOME/.config/censorprivacy/managed" -- '--help 是任务文本。'
```

完整 standalone 包根目录的 `cpctl` 额外支持 `verify` 和 `self-test`。源码与普通安装的 `cpctl` 没有这两个子命令；详见 [包说明](binary-distribution.md)。

## 3. 模型、安装与打包

### 模型工具

```bash
python3 -B scripts/model/export.py --help
python3 -B scripts/model/export.py --source /path/to/source --check-source
.venv-model/bin/python -B scripts/model/export.py \
  --source /path/to/source --output models/bert4ner-base-chinese
```

`--help` 与 `--check-source` 只需 Python ≥3.11 标准库；真正导出还需 [模型工具依赖](../scripts/model/README.md)。工具只读取本地源模型，不会下载权重。联网下载由前置的 `hf download` 完成，固定 revision 见 [构建文档](build-and-deploy.md#41-下载原始模型)。

### 底层安装

```bash
bash scripts/install.sh --prefix "$HOME/.local"
```

可选 `--with-native` 构建安装 OpenSSL hook。`DESTDIR` 用于暂存，不改变最终前缀：

```bash
DESTDIR=/tmp/censorprivacy-stage bash scripts/install.sh --prefix /usr/local
```

上例输出到 `/tmp/censorprivacy-stage/usr/local/`。脚本始终执行 release 构建，默认前缀 `/usr/local`；而 `cpctl install` 默认 `~/.local`，并处理复制提权和安装位置记录。两者均不安装模型或 ONNX Runtime。

### 打包

```bash
bash scripts/package.sh
bash scripts/package.sh --with-native
```

两条命令分别生成基础安装归档和附带 native 的基础归档，不包含 ONNX/运行库及完整系统依赖。需要完整开箱包时使用 `--standalone`，参数与许可布局见 [打包指南](binary-distribution.md#4-从源码打包)。

`scripts/release/package.sh`、`assemble.mjs`、`launcher.sh` 是完整包内部实现，通过 `scripts/package.sh --standalone` 调用即可。

## 4. 测试与验收

先选择验证层级，避免把不具备环境的扩展测试当成默认要求。模型导出、Cargo 构建、多实例 ONNX 与浏览器验收会占用较多内存；资源较小时使用 `CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1` 并串行执行。

### 默认与检测器回归

| 命令 | 资源要求 | 覆盖 |
| --- | --- | --- |
| `bash scripts/test/run-all.sh` | Rust、C 链接器；部分测试需本机监听 | fmt、Clippy、workspace/all-targets 默认测试 |
| `bash scripts/test/rust.sh` | 同上 | 上一命令的默认 Rust 检查入口 |
| `bash scripts/test/detector.sh` | Rust；无需模型或 Python | 原生规则、验证与资源拒绝 |
| `bash scripts/test/detector.sh --integration` | 真实 ONNX/ORT 路径 | 固定参考样本、网关/SSE、共享 TLS 转换、HTTPS CLI |
| `bash scripts/test/operations.sh --static` | Node；无需模型 | Shell/JS 语法及插件边界测试 |

### 环境相关验收

| 命令 | 前置条件 | 覆盖 |
| --- | --- | --- |
| `bash scripts/test/native.sh` | C/OpenSSL 开发环境 | 动态 ABI、分片、重试、curl 和 broker |
| `CP_TEST_REAL_DETECTOR=1 bash scripts/test/native.sh` | 上述条件加真实 ONNX/ORT | native 链路使用真实 NER |
| `bash scripts/test/runtime.sh` | root 与 Linux 隔离能力 | namespace、挂载、网络、清理 |
| `bash scripts/test/operations.sh` | Node、release 构建环境、ONNX/ORT、OpenSSL | 安装、生命周期、网关/TLS 演示 |
| `bash scripts/test/operations.sh --dsh-root /path/to/deepseek-harness` | 上述条件加已构建 DSH | 另加 DSH 两种模拟演示 |

`run-all.sh` 可追加 `--native`、`--runtime`、`--operations`、`--detector`、`--detector-integration`，分别运行对应层级；这些选项会引入上表条件，不会自动准备环境。

### 浏览器与交付包

```bash
# 隐私检测台：先构建 debug 二进制，配置 ONNX/ORT 与 Playwright/Chromium
node scripts/test/detector-web.mjs target/debug/censorprivacy

# 完整归档已解压；使用包内 Node 运行源码中的验收脚本
/path/to/extracted/runtime/bin/node scripts/test/standalone.mjs /path/to/extracted

# 完整包 + 已构建 DSH + 可用浏览器
node scripts/test/dsh-web.mjs /path/to/extracted /path/to/deepseek-harness

# 完整包 + 实际 xiaoO 二进制，本机模拟上游
node scripts/test/xiaoo-gateway.mjs /path/to/extracted /path/to/xiaoo
```

检测台脚本可用 `CP_PLAYWRIGHT_MODULE` 指定本地 Playwright 模块目录，`CHROMIUM_EXECUTABLE` 指定浏览器，`CP_WEB_SCREENSHOT_DIR` 指定截图目录（默认 `.artifacts/detector-web/`）。DSH 浏览器脚本从 Harness 依赖加载 Playwright，可用 `CP_BROWSER_EXECUTABLE` 指定浏览器。浏览器与系统依赖须自行准备。

`xiaoo-gateway.mjs` 追加第 3 个参数 `/绝对路径/managed` 时复用该目录的云连接与凭据，另建独立网关，会实际调用模型。其余上述浏览器/包验收默认使用本机模拟上游。实际执行范围与历史结果见 [验收记录](validation.md)。

