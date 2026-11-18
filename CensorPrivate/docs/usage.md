# 使用手册

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [脚本速查](scripts.md) · [故障排查](troubleshooting.md)

本页按 **检测 → 网关 → Agent → 日常管理** 排列。首次获取源码或准备模型，先完成 [构建与部署](build-and-deploy.md)；已收到完整包，先完成 [解压自检](binary-distribution.md)。

源码和完整包目录均用 `./cpctl`。源码安装后可用 `~/.local/bin/cpctl`；加入 PATH 后可直接用 `cpctl`。除包内专有的 `verify/self-test` 外，下文短命令共用同一套私有配置。

- [1. 打开隐私检测台](#1-打开隐私检测台)
- [2. 启动实际模型网关](#2-启动实际模型网关)
- [3. 接入 Agent](#3-接入-agent)
- [4. 日常管理](#4-日常管理)
- [5. 本机演示与验收](#5-本机演示与验收)
- [6. 可选模式](#6-可选模式)
- [7. 配置与行为参考](#7-配置与行为参考)

## 1. 打开隐私检测台

源码构建后，在已配置模型和运行库的终端执行：

```bash
./target/release/censorprivacy web
```

完整包中使用 `./bin/censorprivacy web`；源码安装后使用 `~/.local/bin/censorprivacy web`。开发调试也可执行：

```bash
CARGO_BUILD_JOBS=1 cargo run --locked --bin censorprivacy -- web
```

浏览器打开终端打印的 `http://127.0.0.1:8766/`，粘贴文本并点击检测。页面展示 `REGEX / RULES / NER` 的命中、原文高亮和三种遮蔽预览；无需配置上游或密钥。更换端口用 `web --port 8767`，按 Ctrl+C 停止。

服务器通过 SSH 使用时，在自己电脑的终端建立转发，再打开本机地址：

```bash
ssh -N -L 8766:127.0.0.1:8766 用户@服务器IP
```

检测页面与聊天网关独立。预览引用不能用于网关恢复；详细参数与范围见 [检测台说明](../plugins/detector-web/README.md)。

## 2. 启动实际模型网关

### 2.1 初始化连接

下面以 DeepSeek 为例；其他服务替换 origin 与实际模型 ID：

```bash
./cpctl init --upstream https://api.deepseek.com --model-id deepseek-flash
```

按提示输入 provider API Key，输入不回显。上游只填 **origin**，不附加 `/v1`、其他路径或查询参数。也可运行 `./cpctl init` 交互填写全部连接信息。

已有当前用户所有的 0600 凭据文件时，追加 `--key-file /绝对路径/upstream.key`；受信任的无认证本机上游可用 `--no-key`。已有配置不会被再次 `init` 覆盖。

### 2.2 配置业务策略

初始化后编辑 `~/.config/censorprivacy/managed/policy.yaml`，补充业务词典和秘密。例如替换其中的 `dictionaries` 段：

```yaml
dictionaries:
  PERSON: [示例联系人]
  ORG: [示例客户甲]
  SECRET: [DEMO_SECRET_DO_NOT_SEND]
```

保留文件中的其他策略项。内置规则和 NER 负责通用检测，业务秘密仍需按实际场景配置。不要把上游 Key 写入策略或 Agent 工作目录。

### 2.3 启动并检查

```bash
./cpctl start
./cpctl check
./cpctl check --model
```

预期网关地址为 `http://127.0.0.1:8775`。源码安装方式启动时，当前终端须仍配置 `CP_DETECTOR_MODEL_DIR` 与 `CP_DETECTOR_ORT_LIBRARY`；完整包自动定位包内资源。

`check` 验证本机认证、权限、版本和 run ID；`check --model` 额外发送实际 Chat Completions 请求，**会使用真实模型并可能计费**。成功后再接入 Agent。模型请求失败时见 [上游与协议排查](troubleshooting.md#3-上游与协议)。

## 3. 接入 Agent

保持第 2 节的网关运行，选择一种客户端。DSH、xiaoO 需另行获取和构建，本项目不会自动下载或升级它们。

### 3.1 通用客户端

| 配置项 | 填写内容 |
| --- | --- |
| Chat Completions Base URL | `http://127.0.0.1:8775/v1` |
| API Key | `~/.config/censorprivacy/managed/gateway.token` 的内容 |
| 模型 ID | 与 `init --model-id` 一致 |

客户端连接本机网关，provider Key 由网关持有。Responses/Messages 客户端按自身路径拼接方式配置，支持入口见 [接口与显示恢复](#接口与显示恢复)。

### 3.2 DeepSeek Harness：浏览器聊天

先按 [DSH 官方源码运行说明](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.zh.md#run-from-source) 完成 `pnpm install` 和 `pnpm run build`，再执行：

```bash
./cpctl dsh web --dsh-root /path/to/deepseek-harness
```

打开日志中**带 token 的完整网址**，默认端口为 `3080`。首次点击“继续”，选择测试工作目录，确认模型为 `CensorPrivacy / <已配置模型ID>`。模型和凭据自动取自当前网关，无需再次填写 API Key。

在聊天框发送下列任务，并粘贴 [虚构项目邮件](../examples/privacy-business.txt)：

```text
请逐字复述以下邮件，保留段落和完整占位符（包括尖括号），使用普通文本，不使用代码块、表格或链接，不计算金额。
```

普通隐私应在本机显示中恢复，`DEMO_SECRET_DO_NOT_SEND` 保持遮盖。模型必须完整保留占位符才能恢复。SSH 服务器另在自己电脑执行：

```bash
ssh -N -L 3080:127.0.0.1:3080 用户@服务器IP
```

PATH 已有可用 `dsh` 时可省略 `--dsh-root`。更换端口用 `--port 3081`；Ctrl+C 只停止 Web，网关继续运行。该 Web 是聊天入口，与第 1 节的检测台不同。

### 3.3 DeepSeek Harness：命令行与工具验收

```bash
./cpctl dsh setup --dsh-root /path/to/deepseek-harness
./cpctl dsh run '请整理以下虚构交接信息，保留完整占位符，使用普通文本：张三在清华大学工作，电话13800138000，测试报告待确认。'
./cpctl dsh cloud
```

`setup` 准备独立 profile 并记住 DSH 路径；`run` 执行实际任务。任务含命令行选项时用 `--` 分隔，例如 `./cpctl dsh run -- '--help 是任务文本，请解释。'`。

`cloud` 使用独立临时网关和虚构策略验证真实模型的 `private_compute sum` 工具调用，预期恢复客户 `DemoCustomerOrchid` 与合计 `300.00`，SECRET 不恢复。报告位于私有配置目录的 `dsh/cloud-report.json`。`run/cloud` 均会调用真实模型。

### 3.4 openEuler xiaoO：终端与文件任务

从 [xiaoO 官方仓库](https://atomgit.com/openeuler/xiaoO) 获取源码，使用 Rust **≥1.88** 在其仓库执行：

```bash
CARGO_BUILD_JOBS=1 cargo build --locked --release -p xiaoo-endside
```

回到 CensorPrivate 或完整包目录，启动终端界面：

```bash
./cpctl xiaoo tui --xiaoo-root /path/to/xiaoO
```

在 TUI 中粘贴同一封虚构邮件。已有 `xiaoo` 命令可省略源码目录；自定义构建位置用 `--xiaoo-bin /绝对路径/xiaoo`，成功后会记住路径。

```bash
./cpctl xiaoo demo
./cpctl xiaoo run '读取 privacy-business.txt，概括交付安排，不计算金额。'
```

`demo` 让真实 Agent 调用 `file_read` 读取虚构邮件并复述；`run` 执行日常任务，也可从 stdin 读取文本。只准备配置用 `./cpctl xiaoo setup`。这些命令复用网关模型与凭据，原 xiaoO 配置保留。

## 4. 日常管理

### 检查、停止与重启

```bash
./cpctl check
./cpctl stop
./cpctl start
```

每次重启产生新 run，原值映射清空。重启后重新打开 Agent 并新建会话；DSH headless 接入重新执行 `dsh setup`。默认 `ttl_seconds: 3600` 表示 run 自启动起一小时有效，到期后进程可能仍在，但请求被拒绝；处理方式见 [有效期排查](troubleshooting.md#2-认证与实例)。

系统服务使用 `systemd start/check/stop`，部署过程见 [构建与部署](build-and-deploy.md#6-部署为系统服务)。同一实例选择一种托管方式。

### 多实例

初始化、启动、检查和停止均使用同一个绝对配置目录，初始化时分配不同端口：

```bash
./cpctl init --config-dir "$HOME/.config/censorprivacy/test" --port 8776
./cpctl start --config-dir "$HOME/.config/censorprivacy/test"
./cpctl check --config-dir "$HOME/.config/censorprivacy/test"
./cpctl stop --config-dir "$HOME/.config/censorprivacy/test"
```

Agent、演示以外的日常命令和 systemd 命令也应带上该实例的 `--config-dir`。

## 5. 本机演示与验收

**无需云凭据、无需先执行 `init/start`** 的本机演示：

```bash
./cpctl demo gateway
```

该命令运行真实 Rust/ONNX 检测器与模拟上游，展示项目邮件的检测、上游占位符、本机恢复和引用求和 `16000.00`；SECRET 始终遮盖。结束后自动停止临时进程，失败时保留私有诊断目录。需要 Node，运行前可用 `./cpctl doctor --demo` 检查。

其他验收按需执行：

| 命令 | 验证内容 | 前置条件 |
| --- | --- | --- |
| `./cpctl demo tls` | 原 OpenSSL 连接、等长替换与恢复 | native、动态 OpenSSL curl、Node |
| `./cpctl demo dsh-gateway --dsh-root /path/to/deepseek-harness` | 真实 Agent 与模拟上游的工具往返 | 已构建 DSH、Node |
| `./cpctl demo dsh-tls --dsh-root /path/to/deepseek-harness` | DSH 的 TLS direct 适配 | 上述条件加 native/OpenSSL |
| `./cpctl self-test` | 完整包校验及网关/TLS 演示 | **仅完整 standalone 包**提供 |
| `./cpctl check --model` | 当前配置的实际模型连通性 | 已初始化并启动；可能计费 |
| `./cpctl dsh cloud` | 实际模型的引用计算与最终显示 | 当前服务、DSH 和有效云凭据；可能计费 |

本机模拟演示可以观察实际发给测试上游的内容；真实模型验收检查工具与显示结果。测试不证明任意业务召回率，也不能观测云服务内部存储。开发回归入口见 [脚本速查](scripts.md#4-测试与验收)。

## 6. 可选模式

### TLS direct

希望保留受支持客户端的原 TLS 连接时，按 [native 说明](../native/README.md) 构建 hook，再以独立配置使用 `init --mode tls`。该模式需要匹配证书的固定 HTTPS IPv4 origin、动态 OpenSSL、HTTP/1.1、Content-Length、非流式未压缩 JSON。

本机示例用 `./cpctl demo tls`；实际模型检查用 `dsh cloud`，不使用 `check --model`。普通网关、检测页面及 xiaoO 接入无需 native。

底层接入时，先用 `censorprivacy serve --tls-direct` 启动 broker，再用 `tls-run` 启动协作客户端：

```bash
tls-run \
  --broker http://127.0.0.1:8775 \
  --token-file "$CONTROL_DIR/gateway.token" \
  --run-state-file "$CONTROL_DIR/run.json" \
  --upstream-key-file "$CONTROL_DIR/upstream.key" \
  --native-library "$PWD/native/libcp_tls_guard.so" \
  --workspace "$AGENT_WORKSPACE" \
  --timeout 120 \
  -- "$CLIENT_EXECUTABLE" CLIENT_ARGS
```

`CONTROL_DIR` 指向实际实例的私有配置目录，`AGENT_WORKSPACE` 是客户端工作目录；可执行文件及参数替换为实际程序。此示例从 CensorPrivate 源码目录运行，native 库须先构建；安装后将库路径改为可信安装位置。TLS 客户端自行完成正确的 CA 与目标身份校验，launcher 不建立强制网络隔离域。

### Linux 受管运行域

需要约束 Agent 绕过网关的独立网络出口时，使用 `privacy-run`。它另需 root、namespace/mount/nft 能力及受控工作目录。`demo`、DSH profile 和 `tls-run` 本身不建立这一运行域。

```bash
sudo privacy-run \
  --run-state-file "$CONTROL_DIR/run.json" \
  --token-file "$CONTROL_DIR/gateway.token" \
  --workspace "$AGENT_WORKSPACE" \
  --uid "$AGENT_UID" --gid "$AGENT_GID" \
  --gateway-port 8775 \
  --read-only-path "$AGENT_RUNTIME" \
  --timeout 300 \
  -- "$AGENT_EXECUTABLE" AGENT_ARGS
```

上例变量须设置为实际控制目录、专属工作区、非 root UID/GID、可信只读运行依赖及可执行文件。launcher 的控制文件必须由 root 或目标 UID 持有；服务账户不同时由管理员提供独立受控文件。网关须能从宿主 readiness 探测和受管 veth 两侧访问，单独监听宿主 loopback 不足以服务运行域。内核还需支持 `mount_setattr`、`close_range`、pidfd 和密封 memfd；依赖准备见 [构建参考](build-and-deploy.md#用途与依赖)。

### 私密计算与 MCP

DSH profile 已注册 `private_compute`。其他客户端可配置 `privcalc` 或 stdio MCP，提供当前实例的 `CENSORPRIVACY_URL`、`CENSORPRIVACY_TOKEN` 和 `CENSORPRIVACY_RUN_ID`。这些环境由 launcher 或可信控制面提供，TLS direct 模式另需 `CENSORPRIVACY_MODE=tls-direct`。

```bash
privcalc aggregate "$REF_A" "$REF_B"
```

`REF_A`、`REF_B` 必须为当前实例已签发且授权有效的引用。CLI 支持 `sum`、`mean`、`min`、`max`、`aggregate`、`date_diff`；工具调用本机 `/v1/private_compute`，返回继承敏感标签的派生引用。stdio MCP 入口为 `censorprivacy-mcp`，在客户端已有 MCP 配置中注册，实际操作由 `tools/list` 返回。普通 Shell/Python 工具不获得原值读取权限。

## 7. 配置与行为参考

### 私有文件

默认目录 `~/.config/censorprivacy/managed/` 为 0700，内部文件为 0600：

| 文件或目录 | 用途 |
| --- | --- |
| `config.json` | 模式、上游 origin、模型、端口、native/CA 路径 |
| `policy.yaml` | 本地业务策略 |
| `gateway.token`、`curl.headers` | 本机客户端认证 |
| `upstream.key` | 可选 provider Key |
| `run.json` | 当前 run ID，不能据此恢复内存映射 |
| `service.log`、`probe-response.json` | 启动诊断与实际模型检查结果 |
| `dsh/`、`dsh-web/` | 独立 DSH profile、工作区及验收报告 |
| `xiaoo/` | 独立 xiaoO 配置、已记住的路径和工作区 |

配置由 JSON 读取，不作为 Shell 执行。`init` 保留已有配置；更新连接配置或策略后停止并重启，系统服务重新执行 `systemd install`。

### 接口与显示恢复

支持 `/v1/chat/completions`、`/v1/responses`、`/v1/messages`、`/v1/messages/count_tokens`。普通网关支持已实现的 JSON 与文本 SSE 格式。只有授权的被动显示位置可以恢复；代码、链接目标、普通工具参数和未知通道不会恢复原值。

网关对未指定 `thinking` 的 `deepseek-*` Chat Completions 请求补充 `{"type":"disabled"}`。显式开启思考或非空 `reasoning_content` 会被拒绝；TLS direct 字节流不自动添加该字段。

### Agent 接入范围

DSH 使用独立 profile，演示仅开放 `private_compute`；支持非 reasoning Chat Completions 与工具调用，未知内容块拒绝处理。插件字段见 [DSH 插件说明](../plugins/dsh-censorprivacy/README.md)。

xiaoO 使用网关与原生 SSE，配置的 `api_base` 是完整 `/v1/chat/completions` 地址。普通工具参数不恢复隐私原值，尚未注册 DSH 的 `private_compute`。`demo` 仅开放 `file_read`，`run/TUI` 的工具权限由实际运行环境管理。首次检测完整系统提示和工具目录可能较慢，应等待完成。
