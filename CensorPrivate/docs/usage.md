# 使用手册

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [脚本速查](scripts.md) · [故障排查](troubleshooting.md)

本页按 **检测 → 网关 → Agent → 日常管理** 排列。首次获取源码或准备模型，先完成 [构建与部署](build-and-deploy.md)；已收到完整包，先完成 [解压自检](binary-distribution.md)。

源码和完整包目录均用 `./censorprivacy`。源码安装后可用 `~/.local/bin/censorprivacy`；加入 PATH 后可直接用 `censorprivacy`。除包内专有的 `verify/self-test` 外，下文短命令共用同一套私有配置。

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
./censorprivacy init --upstream https://api.deepseek.com --model-id deepseek-flash
```

按提示输入 provider API Key，输入不回显。上游只填 **origin**，不附加 `/v1`、其他路径或查询参数。也可运行 `./censorprivacy init` 交互填写全部连接信息。

已有当前用户所有的 0600 凭据文件时，追加 `--key-file /绝对路径/upstream.key`；受信任的无认证本机上游可用 `--no-key`。已有配置不会被再次 `init` 覆盖。

### 2.2 配置业务策略

编辑源码根目录 `config.yaml`（安装版为 `share/censorprivacy/config.yaml`），业务词典和秘密直接写入 `custom`，例如：

```yaml
server:
  ttl_seconds: 86400
detection:
  categories: all
custom:
  epoch: 1
  dictionaries:
    PERSON: [示例联系人]
    ORG: [示例客户甲]
    SECRET: [DEMO_SECRET_DO_NOT_SEND]
```

保留文件中的其他策略项。内置规则和 NER 负责通用检测，业务秘密仍需按实际场景配置。不要把上游 Key 写入策略或 Agent 工作目录。

部署参数和类别选择统一放在源码目录的 `config.yaml`。随附配置设置 `detection.categories: all`，开启全部类别；填写列表则只检测所列类别，省略该项、填写 `null` 或 `[]` 则关闭按类别检测。业务规则放在 `custom` 下，TTL 在 `server.ttl_seconds` 中设置，NER 参数在 `ner` 下管理。详细字段、优先级与容器挂载见 [统一配置](configuration.md)。

当前有 16 个内置类别：`ADDRESS` 专指通讯/邮寄地址，`IP_ADDRESS` 独立检测 IP；`MEDICAL_INFO` 已移除，旧类别列表需删去该项。NER 同时服务 `PERSON / ORG / LOCATION / DATE`，四类全部关闭后自动跳过模型加载与推理，仅关闭前三类而保留 DATE 仍会运行模型。规则模式示例及性能边界见 [类别选择与 NER 开销](configuration.md#类别选择与-ner-开销)。

### 2.3 启动并检查

```bash
./censorprivacy start
./censorprivacy check --model
```

预期网关地址为 `http://127.0.0.1:8775`。源码目录中的 `config.yaml` 已填写本机检测资源路径；更换模型时修改 `ner.model_dir` 和 `ner.ort_library`，模型目录内各自的配置文件继续保留。安装版可配置这些路径或使用 `CP_DETECTOR_MODEL_DIR` 与 `CP_DETECTOR_ORT_LIBRARY`；完整包使用包内资源。

网关同时提供隐私检测台，执行 `./censorprivacy dashboard` 查看地址。通过 SSH 使用服务器时，在自己电脑的终端执行以下命令并保持运行，然后浏览器打开 `http://127.0.0.1:8775/`：

```bash
ssh -N -L 8775:127.0.0.1:8775 用户@服务器IP
```

这里使用当前网关的端口。第 1 节的 8766 属于单独启动的检测台；已有网关时可以直接使用 8775 页面。

`check` 验证本机认证、权限、版本和 run ID；`check --model` 额外发送实际 Chat Completions 请求，**会使用真实模型并可能计费**。成功后再接入 Agent。模型请求失败时见 [上游与协议排查](troubleshooting.md#3-上游与协议)。

## 3. 接入 Agent

保持第 2 节的网关运行，选择一种客户端。DSH、xiaoO 需另行安装，本项目不会自动下载或升级它们。

### 3.1 通用客户端

| 配置项 | 填写内容 |
| --- | --- |
| Chat Completions Base URL | `http://127.0.0.1:8775/v1` |
| API Key | `~/.config/censorprivacy/managed/gateway.token` 的内容 |
| 模型 ID | 与 `init --model-id` 一致 |

客户端连接本机网关，provider Key 由网关持有。Responses/Messages 客户端按自身路径拼接方式配置，支持入口见 [接口与显示恢复](#接口与显示恢复)。

### 3.2 DeepSeek Harness：浏览器聊天

兼容性验证基线为 **DSH `0.2.2-rc.1`**，对应标签 `dsh-v0.2.2-rc.1`、提交 `66491b05e3f05ad35bcdd4f6cd7ed26041d9e3db`。DSH 需要 **Node >=24.9.0**；完整包内置相容运行时。推荐直接安装官方 npm 包：

```bash
npm install --prefix /path/to/dsh-runtime @deepseek-ai/dsh@0.2.2-rc.1
/path/to/dsh-runtime/node_modules/.bin/dsh --version
./censorprivacy dsh web --dsh-path /path/to/dsh-runtime
```

`--dsh-path` 接受 npm 安装前缀、`@deepseek-ai/dsh` 包目录、`node_modules/.bin/dsh` 或已构建源码目录。已有源码目录先确认没有需要保留的改动，再按以下步骤准备：

```bash
cd /path/to/deepseek-harness
git fetch origin tag dsh-v0.2.2-rc.1
git switch --detach dsh-v0.2.2-rc.1
pnpm install --frozen-lockfile
pnpm run build
node apps/cli/lib/bin.js --version  # 基线环境输出 0.2.2-rc.1
```

依赖和构建要求见 [该版本官方源码运行说明](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.1/README.zh.md#run-from-source)。回到 `CensorPrivate/` 目录后执行：

```bash
./censorprivacy dsh web --dsh-path /path/to/deepseek-harness
```

打开日志中**带 token 的完整网址**，默认端口为 `3080`。首次点击“继续”，选择测试工作目录，确认模型为 `CensorPrivacy / <已配置模型ID>`。模型和凭据自动取自当前网关，无需再次填写 API Key。

首次直接访问 `http://127.0.0.1:3080/` 会返回 401，必须先使用日志中的 token 网址。Web 重启后使用新的网址；网址中的 token 由 Harness 生成，与网关的 `gateway.token` 不同。

顶栏“轨迹”后的“隐私”显示当前会话的输入遮蔽对比。发送消息后，可切换请求轮次与系统提示、用户输入、助手上下文或工具结果，查看原文敏感范围和当次真正发给模型的遮蔽文本。页面自动同步、刷新后可回看；窄屏上下排列。数据复用网关记录，无需另开检测台或再加载模型。需使用 gateway 模式并启用 `conversation-records-dir`（`censorprivacy start` 默认启用）；TLS direct 与接入前的历史没有对比记录。升级插件后重启 Harness Web 即可显示入口。独立插件构建与打包见 [插件说明](../plugins/dsh-censorprivacy/README.md)。

在聊天框发送下列任务，并粘贴 [虚构费用工单](../examples/demo.txt)：

```text
请逐字复述以下工单，保留段落和完整占位符（包括尖括号），使用普通文本，不使用代码块、表格或链接，不计算金额。
```

普通隐私应在本机显示中恢复，`DEMO_SECRET_DO_NOT_SEND` 保持遮盖。模型必须完整保留占位符才能恢复。SSH 服务器另在自己电脑执行：

```bash
ssh -N -L 3080:127.0.0.1:3080 用户@服务器IP
```

PATH 已有可用 `dsh` 时可省略 `--dsh-path`；旧参数 `--dsh-root` 仍可用。更换端口用 `--port 3081`；Ctrl+C 只停止 Web，网关继续运行。该 Web 是聊天入口，与第 1 节的检测台不同。

`censorprivacy` 不限制 DSH 版本。更换版本后可运行 `bash scripts/test/operations.sh --dsh-path /path/to/deepseek-harness` 复验安装、插件及 gateway/TLS 工具往返；浏览器入口另用 `scripts/test/dsh-web.mjs` 验证，见 [测试入口](scripts.md#4-测试与验收)。源码切换后需重新构建。

### 3.3 DeepSeek Harness：命令行与工具验收

```bash
./censorprivacy dsh setup --dsh-path /path/to/deepseek-harness
./censorprivacy dsh run '请整理以下虚构交接信息，保留完整占位符，使用普通文本：张三在清华大学工作，电话13800138000，测试报告待确认。'
./censorprivacy dsh cloud
```

`setup` 准备独立 profile 并记住 DSH 路径；`run` 执行实际任务。任务含命令行选项时用 `--` 分隔，例如 `./censorprivacy dsh run -- '--help 是任务文本，请解释。'`。

`run` 和 `web` 默认同时提供 DSH 自带工具和 CensorPrivacy 私密工具。原生工具按 DSH 自身权限运行，不会获得替换隐私引用所需的原值。仅需私密工具时，在本次启动加 `--disable_tool`：

```bash
./censorprivacy dsh web --dsh-path /path/to/deepseek-harness --disable_tool
```

该开关仅对本次运行生效，下一次启动仍默认提供原生工具。旧参数 `--enable-tools`、`--no-tools` 继续兼容。

`cloud` 使用独立临时网关和虚构策略验证真实模型的 `private_compute sum`，预期恢复客户 `DemoCustomerOrchid` 与合计 `300.00`，SECRET 不恢复。它从当前网关的认证 `/ready` 继承实际 NER 线程、设备和 batch 设置（包括 `start` 临时覆盖），并将请求值与实际值写入私有的 `dsh/cloud-report.json`。设置不一致会在请求模型前停止。`run/cloud` 均会调用真实模型；`cloud` 验收期间会额外加载一份检测模型，请确认本机有足够资源。

### 3.4 openEuler xiaoO：终端与文件任务

从 [xiaoO 官方仓库](https://atomgit.com/openeuler/xiaoO) 获取源码，使用 Rust **≥1.88** 在其仓库执行：

```bash
CARGO_BUILD_JOBS=1 cargo build --locked --release -p xiaoo-endside
```

回到 `CensorPrivate/` 或完整包目录，启动终端界面：

```bash
./censorprivacy xiaoo tui --xiaoO-path /path/to/xiaoO
```

默认关闭 xiaoO 自身的工具，适合普通文本对话。需要工具时运行 `./censorprivacy xiaoo tui --enable-tools --xiaoO-path /path/to/xiaoO`：启动器会先加载真实工具目录并预热本地隐私检测，显示准备进度后才进入界面。准备请求不访问云模型、不进入 Web 记录；新的用户文本仍会正常检测。

在 TUI 中粘贴同一份虚构工单。`--xiaoO-path` 指定源码目录，依次查找 `target/release/xiaoo`、`target/debug/xiaoo`，成功后会记住可执行文件路径。已有 `xiaoo` 命令可省略此参数；旧参数 `--xiaoo-root` 和 `--xiaoo-bin` 仍兼容。

终端同时输出隐私检测台地址，默认 `http://127.0.0.1:8775/`，端口与当前网关一致。也可运行 `./censorprivacy xiaoo web` 查看地址。网页自动同步每次启动的会话，显示用户输入、Agent 回复、工具调用和工具结果；点击消息下的“查看检测与遮蔽”，查看发送前原文、命中位置及当次真正发给上游的遮蔽文本。记录详情只读，点击“返回独立检测”可继续使用原来的文本检测功能。

更新前已启动的网关需要先 `./censorprivacy stop` 再 `./censorprivacy start`，再新建 xiaoO 会话。网关和网页共用一个本地模型实例，无需另外启动 `censorprivacy web`。浏览器刷新和网关重启后均可回看记录；旧 CP 引用仅供回看，不能跨网关 run 使用。

会话包含原始隐私文本，用户实例保存在当前配置目录的 `xiaoo/records/`（systemd 实例位于 `/var/lib/censorprivacy/<实例名>.xiaoo-records/`），目录权限 0700、文件权限 0600；带 Agent 会话标识的请求（包括 Harness 自动传递的标识）会被保存，普通网关请求与独立检测文本不保存。记录不自动过期。测试前可点击对话记录右上角的“清空记录”，确认后永久删除当前实例的全部会话及检测详情，页面同步恢复空白；刷新或重启不会恢复，清空前已开始的请求也不会重新写回旧记录。此操作保留网关策略和凭据，不清除 xiaoO 自身上下文；开始新一轮测试请退出并重新运行 `./censorprivacy xiaoo tui`，避免原对话历史随新请求再次发送。网页仅允许本机同源访问。

```bash
./censorprivacy xiaoo demo --enable-tools
./censorprivacy xiaoo run '请整理以下交接信息：……'
```

`demo` 让真实 Agent 调用 `file_read` 读取虚构工单并复述；`run` 执行日常任务，也可从 stdin 读取文本。只准备配置用 `./censorprivacy xiaoo setup`。这些命令复用网关模型与凭据，原 xiaoO 配置保留。

只有 `demo` 会检查并复制 `examples/demo.txt` 到独立工作区的 `demo.txt`，再要求 Agent 读取它；删除样本后仍可使用 `setup`、`tui` 和 `run`。普通启动不读取或复制演示文件。开启工具时的启动预热处理工具定义，不加载演示文件内容。DSH 插件及日常 `dsh setup/run/web` 同样不依赖此文件。

### 3.5 通用 Agent 会话检测台

`./censorprivacy start` 启动的网关也提供隐私检测台，运行 `./censorprivacy dashboard` 查看地址（默认 `http://127.0.0.1:8775/`）。通过当前 CensorPrivacy 插件运行的 DeepSeek Harness CLI/Web 会自动按真实会话标识同步，页面标明来源，支持流式回复、工具结果和点击查看当次检测与遮蔽。xiaoO 历史与新记录可在同一页面查看。

其他 OpenAI Chat Completions Agent 可设置 Base URL 为 `http://127.0.0.1:8775/agents/my-agent/session-001/v1`；若需要完整端点则加 `/chat/completions`。也可继续使用标准端点并增加 `X-CensorPrivacy-Agent: my-agent`、`X-CensorPrivacy-Session: session-001` 两个请求头。两种方式选一种；同一会话复用标识，新会话换标识。来源最多 64 字符、会话最多 128 字符，均使用 ASCII 字母、数字、`-_.:`。来源和会话参数只在本机使用，不发给模型。

底层 `serve/web` 使用 `--conversation-records-dir /绝对路径`；旧 `--xiaoo-records-dir` 参数与记录目录继续兼容。未携带会话标识的请求不记录；不扫描、导入之前的 Harness 原生日志。当前记录支持 gateway 的 Chat Completions，TLS direct 及 Messages/Responses 协议尚不提供会话记录。更多接入说明见 [检测台说明](../plugins/detector-web/README.md)。

## 4. 日常管理

NER 参数统一在 `config.yaml` 的 `ner` 中持久配置，随附文件使用 CPU、4 个 intra-op 线程、batch 1、每窗口 16 秒；省略线程数时回退到实例设置，内置默认 1，inter-op 固定为 1。更改后停止并重新启动网关。`start --ner-threads N` 可临时覆盖 YAML，优先级见 [统一配置](configuration.md)。

GPU 设备和每卡 batch 可在网关启动时配置，所有 Harness 共用检测核心，分窗保持 512/64：

```bash
./censorprivacy start --ner-device cpu --ner-threads 4
./censorprivacy start --ner-device cuda --ner-batch-size 4
./censorprivacy start --ner-device cuda:0,1,2,3 --ner-batch-size 4
./censorprivacy start --ner-device cann --ner-batch-size 4
```

默认 CPU、batch 1；`cuda` 等同 `cuda:0`，`cann` 等同 `cann:0`，两者均支持多卡列表。`init` 可保存配置，`start` 临时覆盖。CUDA 需要 GPU 原生运行库及匹配驱动/CUDA/cuDNN，完整包默认附带 CPU 运行库；资源设置和多卡行为见 [检测器配置](rust-detector.md#2-配置路径)。昇腾需含 CANN provider 的 ORT、匹配的 Ascend 驱动与 CANN Toolkit；两种加速后端均需在对应硬件上验收。


### 检查、停止与重启

```bash
./censorprivacy check
./censorprivacy stop
./censorprivacy start
```

每次重启产生新 run，原值映射清空。重启后重新打开 Agent 并新建会话；DSH headless 接入重新执行 `dsh setup`。`server.ttl_seconds` 从启动起计时，随附配置为 86400 秒（24 小时），省略时默认 3600 秒（1 小时）。到期后进程可能仍在，但请求被拒绝；处理方式见 [有效期排查](troubleshooting.md#2-认证与实例)。

系统服务使用 `systemd start/check/stop`，部署过程见 [构建与部署](build-and-deploy.md#6-部署为系统服务)。同一实例选择一种托管方式。

### 多实例

初始化、启动、检查和停止均使用同一个绝对配置目录，初始化时分配不同端口：

```bash
./censorprivacy init --config-dir "$HOME/.config/censorprivacy/test" --port 8776
./censorprivacy start --config-dir "$HOME/.config/censorprivacy/test"
./censorprivacy check --config-dir "$HOME/.config/censorprivacy/test"
./censorprivacy stop --config-dir "$HOME/.config/censorprivacy/test"
```

Agent、演示以外的日常命令和 systemd 命令也应带上该实例的 `--config-dir`。

## 5. 本机演示与验收

**无需云凭据、无需先执行 `init/start`** 的本机演示：

```bash
./censorprivacy demo gateway
```

该命令运行真实 Rust/ONNX 检测器与模拟上游，使用 `examples/demo.txt` 展示费用工单的检测、上游占位符、本机恢复和引用求和 `2100.00`；SECRET 始终遮盖。结束后自动停止临时进程，失败时保留私有诊断目录。需要 Node，运行前可用 `./censorprivacy doctor --demo` 检查。

其他验收按需执行：

| 命令 | 验证内容 | 前置条件 |
| --- | --- | --- |
| `./censorprivacy demo tls` | 原 OpenSSL 连接、等长替换与恢复 | native、动态 OpenSSL curl、Node |
| `./censorprivacy demo dsh-gateway --dsh-path /path/to/deepseek-harness` | 真实 Agent 与模拟上游的工具往返 | 已构建 DSH、Node |
| `./censorprivacy demo dsh-tls --dsh-path /path/to/deepseek-harness` | DSH 的 TLS direct 适配 | 上述条件加 native/OpenSSL |
| `./censorprivacy self-test` | 完整包校验及网关/TLS 演示 | **仅完整 standalone 包**提供 |
| `./censorprivacy check --model` | 当前配置的实际模型连通性 | 已初始化并启动；可能计费 |
| `./censorprivacy dsh cloud` | 实际模型的引用计算与最终显示 | 当前服务、DSH 和有效云凭据；可能计费 |

本机模拟演示可以观察实际发给测试上游的内容；真实模型验收检查工具与显示结果。测试不证明任意业务召回率，也不能观测云服务内部存储。开发回归入口见 [脚本速查](scripts.md#4-测试与验收)。

## 6. 可选模式

### TLS direct

希望保留受支持客户端的原 TLS 连接时，按 [native 说明](../native/README.md) 构建 hook，再以独立配置使用 `init --mode tls`。该模式需要匹配证书的固定 HTTPS IPv4 origin、动态 OpenSSL、HTTP/1.1、Content-Length、非流式未压缩 JSON。

本机示例用 `./censorprivacy demo tls`；实际模型检查用 `dsh cloud`，不使用 `check --model`。普通网关、检测页面及 xiaoO 接入无需 native。

底层接入时，先用 `censorprivacy serve --tls-direct` 启动 broker，再用 `censorprivacy tls-run` 启动协作客户端：

```bash
censorprivacy tls-run \
  --broker http://127.0.0.1:8775 \
  --token-file "$CONTROL_DIR/gateway.token" \
  --run-state-file "$CONTROL_DIR/run.json" \
  --upstream-key-file "$CONTROL_DIR/upstream.key" \
  --native-library "$PWD/native/libcensorprivacy_tls_guard.so" \
  --workspace "$AGENT_WORKSPACE" \
  --timeout 120 \
  -- "$CLIENT_EXECUTABLE" CLIENT_ARGS
```

`CONTROL_DIR` 指向实际实例的私有配置目录，`AGENT_WORKSPACE` 是客户端工作目录；可执行文件及参数替换为实际程序。此示例从 `CensorPrivate/` 源码目录运行，native 库须先构建；安装后将库路径改为可信安装位置。TLS 客户端自行完成正确的 CA 与目标身份校验，launcher 不建立强制网络隔离域。

### Linux 受管运行域

需要约束 Agent 绕过网关的独立网络出口时，使用 `censorprivacy run`。它另需 root、namespace/mount/nft 能力及受控工作目录。`demo`、DSH profile 和 `censorprivacy tls-run` 本身不建立这一运行域。

```bash
sudo censorprivacy run \
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

DSH 自动注册 `private_compute` 及已配置的自定义私密工具。其他客户端可配置 `censorprivacy compute` 或 stdio MCP，提供当前实例的 `CENSORPRIVACY_URL`、`CENSORPRIVACY_TOKEN` 和 `CENSORPRIVACY_RUN_ID`。这些环境由 launcher 或可信控制面提供，TLS direct 模式另需 `CENSORPRIVACY_MODE=tls-direct`。

```bash
censorprivacy compute aggregate "$REF_A" "$REF_B"
```

`REF_A`、`REF_B` 必须为当前实例已签发且授权有效的引用。CLI 支持 `sum`、`mean`、`min`、`max`、`aggregate`、`date_diff`；工具调用本机 `/v1/private_compute`，返回继承敏感标签的派生引用。stdio MCP 入口为 `censorprivacy mcp`，在客户端已有 MCP 配置中注册，实际操作由 `tools/list` 返回。普通 Shell/Python 工具不获得原值读取权限。

### 注册自定义私密工具

1. 把 [注册表样例](../examples/private-tools/manifest.example.json) 复制到管理目录下的 `private-tools.json`，权限设为 `0600`。管理目录默认是 `${XDG_CONFIG_HOME:-$HOME/.config}/censorprivacy/managed`，也可用 `--config-dir` 指定。
2. 修改 `command` 中的绝对路径，指向本机可信的 [Python 排序脚本](../examples/private-tools/sort.py) 或 [Shell 排序脚本](../examples/private-tools/sort.sh)。Python 样例使用 Python 3；Shell 样例使用 `jq`。`command` 是固定的程序和参数列表，工具名称使用 `private_*`，不要把密钥、路径或业务数据写进模型可见的 `description`。
3. 可在统一配置的 `private_tools_file` 中填写注册表路径，也可使用上述实例默认位置。重启网关和 DSH，DSH 会自动发现私密工具，使用 `--disable_tool` 时仍会保留。直接运行网关时也可用 `--private-tools-file` 覆盖路径。systemd 用户修改后需要重新执行 `censorprivacy systemd install`，并确保服务账号能读取脚本及依赖。

本机认证接口 `GET /v1/private_tools` 列出工具；`POST /v1/private_tools/<name>` 对自定义工具仅接受 `{"refs":[...]}`。网关验证当前 run 的引用、排除 SECRET，再通过标准输入将 `{"inputs":[{"value":"…","entity_type":"…","unit":null}]}` 交给程序。标准输出必须是单个 JSON 对象：

- `"output": "order"`：输出 `{"order":[1,0]}` 等从零开始的完整排列。网关按该顺序返回原有引用。
- `"output": "values"`：输出 `{"values":["…"]}`。网关把结果注册为继承输入依赖的 `PRIVATE_RESULT` 引用；检测到凭据时拒绝发布。

两种调用结果都只含 `{"refs":[...]}`。每次最多 256 个输入、256 KiB 标准输入和标准输出，单个派生值最多 16 KiB。程序默认 3 秒超时（可配置 100–30000 毫秒），每个网关同一时间只运行一个自定义程序。工具运行时不继承凭据环境，也不能打开网络套接字；Linux 需要 libseccomp。脚本使用网关账号的文件访问权限，因此只应登记经本机管理员审核的程序及其依赖。旧 `private_compute`、`censorprivacy compute` 和 stdio MCP 保持兼容；自定义工具通过 DSH 或上述 HTTP 接口调用。

## 7. 配置与行为参考

### 私有文件

统一部署配置位于源码根目录或安装目录的 `config.yaml`，内含 `custom` 自定义规则。实例状态默认保存在 `~/.config/censorprivacy/managed/`，目录为 0700，内部文件为 0600：

| 文件或目录 | 用途 |
| --- | --- |
| `config.json` | 模式、上游 origin、模型、端口、native/CA 路径 |
| `gateway.token`、`curl.headers` | 本机客户端认证 |
| `upstream.key` | 可选 provider Key |
| `run.json` | 当前 run ID，不能据此恢复内存映射 |
| `service.log`、`probe-response.json` | 启动诊断与实际模型检查结果 |
| `dsh/`、`dsh-web/` | 独立 DSH profile、工作区及验收报告 |
| `xiaoo/` | 独立 xiaoO 配置、已记住的路径、工作区和私有对话记录 |

配置由 JSON 读取，不作为 Shell 执行。`init` 保留已有配置；更新连接配置或策略后停止并重启，系统服务重新执行 `systemd install`。

### 接口与显示恢复

支持 `/v1/chat/completions`、`/v1/responses`、`/v1/messages`、`/v1/messages/count_tokens`。普通网关支持已实现的 JSON 与文本 SSE 格式。只有授权的被动显示位置可以恢复；代码、链接目标、普通工具参数和未知通道不会恢复原值。

网关对未指定 `thinking` 的 `deepseek-*` Chat Completions 请求补充 `{"type":"disabled"}`。显式开启思考或非空 `reasoning_content` 会被拒绝；TLS direct 字节流不自动添加该字段。

### Agent 接入范围

DSH 使用独立 profile，默认同时开放原生工具和私密工具，`--disable_tool` 仅保留私密工具；支持非 reasoning Chat Completions 与工具调用，未知内容块拒绝处理。gateway 模式的 Web 与 `dsh run` 使用真实 SSE，校验后的文字逐段显示；工具调用在完整参数与流结束校验后交给 DSH。TLS direct 保留完整 JSON 校验。插件字段见 [DSH 插件说明](../plugins/dsh-censorprivacy/README.md)。

xiaoO 使用网关与原生 SSE，配置的 `api_base` 是带本次启动会话标识的完整 `/xiaoo/<session>/no-tools/v1/chat/completions` 地址（开启工具时省略 `no-tools/`）；会话标识仅用于本机记录，不发送给上游。TUI 中的模型请求归属本次启动，重新启动 xiaoO 会创建另一条会话。网关记录实际模型交互，未发送给模型的 TUI 编辑、界面命令不在记录范围内。普通工具参数不恢复隐私原值，尚未注册 DSH 的 `private_compute`。`demo --enable-tools` 仅开放 `file_read`。`run/TUI` 默认不向上游提供工具定义，并阻止工具调用；显式启用后，其工具权限由实际运行环境管理。准备将工具目录检测移至进入对话前，耗时仍取决于 CPU 和目录大小；TUI 自身的系统提示与新文本仍需在请求时检测。
