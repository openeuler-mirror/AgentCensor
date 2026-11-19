# 二进制包与交付

[文档首页](README.md) · [使用手册](usage.md) · [构建与部署](build-and-deploy.md) · [脚本速查](scripts.md)

收到 **standalone 完整包**后，按“获取 → 校验 → 解压自检 → 使用”操作。完整包包含 Rust 命令、ONNX 模型、ONNX Runtime、native hook、Node 及运维依赖，运行无需编译或 Python。

- [1. 获取并校验](#1-获取并校验)
- [2. 解压与自检](#2-解压与自检)
- [3. 开始使用](#3-开始使用)
- [4. 从源码打包](#4-从源码打包)
- [5. 交付参考](#5-交付参考)

## 1. 获取并校验

向发布方取得这两个匹配版本的文件：

```text
censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz
censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz.sha256
```

仓库没有内置固定二进制下载地址或自动下载脚本。若发布方提供 HTTPS 目录地址，可以在空目录中执行以下下载命令；直接收到文件则跳过下载：

```bash
read -r -p '发布方提供的 HTTPS 目录地址: ' CP_RELEASE_URL
curl -fLO "${CP_RELEASE_URL%/}/censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz"
curl -fLO "${CP_RELEASE_URL%/}/censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz.sha256"
```

在这两个文件所在目录校验，预期输出 `OK`：

```bash
sha256sum -c censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz.sha256
```

示例为 0.3.2，其他版本同时替换归档名与校验文件名。已验收目标为 **openEuler 24.03 LTS、x86_64、glibc 2.38+**；其他系统先核对包内 `BUNDLE.json` 的实际 ABI 要求。

## 2. 解压与自检

```bash
tar -xzf censorprivacy-0.3.2-linux-x86_64-standalone.tar.gz
cd censorprivacy-0.3.2-linux-x86_64-standalone
./cpctl self-test
```

预期最后输出“完整交付包自检”通过。它依次校验包内文件、检查环境、运行真实本地 NER 与模拟上游的网关/TLS 演示，不调用云模型，结束后自动停止临时子进程。

只检查包内完整性可运行 `./cpctl verify`。这两个命令由**完整包根目录**的 `cpctl` 提供，源码入口和基础安装归档不提供。自检不会启动日常网关，也不会续期已有 run。

包内资源按相对位置定位。初始化前可以整体移动包目录，移动后保留 `bin/`、`lib/`、`runtime/`、`share/` 的布局；不要继承指向其他环境的检测资源变量。

升级或移动已使用的网关前，在旧目录执行 `./cpctl stop`；随后在新目录执行 `./cpctl start`、`./cpctl check`，再重新打开 xiaoO 并新建会话。配置、策略、凭据和已有 Web 记录会保留。0.3.1 修复后续历史消息新增敏感值时第二轮返回 `detection_failed` 的问题，整条请求会统一遮蔽后再接受最终复核。

## 3. 开始使用

先独立查看检测结果，无需云账号：

```bash
./bin/censorprivacy web
```

浏览器打开 `http://127.0.0.1:8766/`，Ctrl+C 停止。随后连接实际服务：

```bash
./cpctl init --upstream https://api.deepseek.com --model-id deepseek-flash
./cpctl start
./cpctl check --model
```

按提示输入自己的 API Key；模型请求可能计费。客户端 Base URL 为 `http://127.0.0.1:8775/v1`，认证使用私有配置目录中的 `gateway.token`。

网关同时在 `http://127.0.0.1:8775/` 提供检测台，共用已加载模型。通过 `./cpctl xiaoo tui` 或 `./cpctl xiaoo run` 启动的会话会自动显示原文、实际遮蔽文本、回复和工具交互。测试前可点击“清空记录”并确认；新包默认没有会话或预填文本，用户记录不会被打入交付包。清空 Web 记录后，需要全新上下文时请重新启动 xiaoO。

接入与日常操作见 [使用手册](usage.md)：

| 目标 | 后续步骤 |
| --- | --- |
| DSH 浏览器聊天 | [DSH Web](usage.md#32-deepseek-harness浏览器聊天) |
| xiaoO 终端与文件任务 | [xiaoO 接入](usage.md#34-openeuler-xiaoo终端与文件任务) |
| 停止、重启、多实例 | [日常管理](usage.md#4-日常管理) |
| 检测页面的 SSH 转发 | [隐私检测台](usage.md#1-打开隐私检测台) |

包根目录 `README.md` 也提供可复制的完整测试邮件及操作说明。DSH、xiaoO 仍需另行安装；完整包不包含它们，也不包含用户凭据。

## 4. 从源码打包

以下命令在 **CensorPrivate 源码目录**执行，面向构建者。

### 4.1 选择归档类型

| 类型 | 命令 | 包含内容 | 使用前还需准备 |
| --- | --- | --- | --- |
| 基础安装包 | `bash scripts/package.sh` | `usr/local/` 布局的 Rust 命令、运维文件和文档 | 模型、ORT、系统运行依赖 |
| 基础安装包 + native | `bash scripts/package.sh --with-native` | 上述内容及 TLS hook | 同上；TLS 客户端兼容环境 |
| standalone 完整包 | `bash scripts/package.sh --standalone …` | 可解压运行的命令、模型、运行库、Node 与依赖 | 目标系统 ABI；实际模型服务凭据 |

前两种包需要按 `usr/local/` 布局安装，不能把解压目录当作 standalone 直接运行。构建安装本身不自动收集模型与 ORT。

### 4.2 准备完整包输入

构建机需要 Rust/C/OpenSSL 开发环境、Node 22.19+（22.x）或 ≥24、patchelf、readelf，以及 jq、curl、openssl、awk、flock。先完成 [源码构建与模型导出](build-and-deploy.md)。

打包器要求原生库和 Node 的许可证位于原发行目录，不能仅传入一份孤立的可执行文件：

| 输入 | 所需布局 |
| --- | --- |
| ONNX 目录 | `manifest.json`、三个受哈希保护的模型文件及 `export-validation.json` |
| ORT wheel 目录 | `onnxruntime/capi/libonnxruntime.so.*`，其上一层 `onnxruntime/` 下有 `LICENSE`、`ThirdPartyNotices.txt` |
| Node 官方发行目录 | `bin/node` 及发行目录根部的 `LICENSE` |

当前组装器按 wheel 的 ORT 许可布局查找文件。使用官方 ORT tar 包时，先按上述布局暂存库和原有许可，或使用准备环境 wheel 的原目录。

若系统 RPM 缺少 Kerberos 许可，打包器会获取固定版本的 NOTICE。离线构建可用 `CP_KRB5_NOTICE_FILE=/绝对路径/NOTICE` 提供缓存副本，仍须通过内置 SHA-256 校验；也可复用已验证完整包中 `share/censorprivacy/licenses/system/krb5-libs/NOTICE`。

### 4.3 执行与验收

```bash
bash scripts/package.sh --standalone \
  --model-dir /path/to/bert4ner-base-chinese-onnx \
  --ort-library /path/to/onnxruntime/capi/libonnxruntime.so.1.31.1 \
  --node /path/to/node/bin/node \
  --patchelf /path/to/patchelf
```

默认输出到 `dist/`，可用 `--output-dir /绝对路径` 指定目录。`--model-dir` 默认使用 `CP_DETECTOR_MODEL_DIR` 或源码 `models/bert4ner-base-chinese/`；`--ort-library` 默认读取 `CP_DETECTOR_ORT_LIBRARY`/`ORT_DYLIB_PATH`。

完整打包包括模型哈希校验、运行依赖与许可收集、ELF 相对库路径处理、源码快照/ABI 记录和内部 `SHA256SUMS`。系统缺失的固定 Kerberos 声明会在打包时联网取得并校验；原始模型和二进制输入不被原地修改。

独立解压后，用包内 Node 运行源码中的验收脚本：

```bash
/path/to/extracted/runtime/bin/node scripts/test/standalone.mjs /path/to/extracted
```

验收覆盖包自检、命令入口、配置凭据、网关保护/恢复、重启与停止。需要验证实际 xiaoO 和 Web 时，可在准备好 Playwright/Chromium 的构建机执行：

```bash
CP_TEST_BUNDLE=/path/to/extracted \
  /path/to/extracted/runtime/bin/node scripts/test/xiaoo-web.mjs \
  /path/to/extracted/bin/censorprivacy /path/to/xiaoo
```

此模式使用包内运维入口和模型，测试配置及记录保存在独立临时目录，上游为本机模拟服务。浏览器和 Python PTY 驱动仅为验收工具，不是交付包运行依赖。保存归档、外部 SHA-256 和对应验收结果；历史交付的结果不代表刚生成的新包已通过验收。

目录迁移验收使用 `scripts/test/relocation.mjs /path/to/extracted /path/to/report.json`，用包内 Node 和普通用户运行；解压目录的父目录需要允许该用户重命名。它先初始化并停止网关，再将整个包移入含空格和中文的新路径，检查既有配置、真实检测、恢复和完整网关/TLS 自检，完成后恢复原目录名。

## 5. 交付参考

| 位置 | 用途 |
| --- | --- |
| `cpctl` | 完整包入口，提供 `verify/self-test` 并转发日常运维命令 |
| `bin/` | 五个 Rust 命令及安装后的运维入口 |
| `lib/censorprivacy/` | ONNX Runtime、native hook、随包共享库 |
| `runtime/bin/` | Node 及运维工具 |
| `share/censorprivacy/` | 模型、文档、插件、CA、许可和 `BUNDLE.json` |
| `SHA256SUMS` | 包内文件校验清单 |

基础包的 `.sha256` 由源码根目录生成，清单中的文件名带 `dist/`；在源码根目录执行 `sha256sum -c dist/对应文件.sha256`。完整包的外部清单只记录归档文件名，在归档所在目录校验即可。

完整包不包含 Python 迁移资料、Rust 源码或开发虚拟环境。`privacy-run` 需要额外 root/namespace 等系统能力；DSH 与 xiaoO 的安装也属于单独步骤。平台和已执行的验证范围见 [验收记录](validation.md)。
