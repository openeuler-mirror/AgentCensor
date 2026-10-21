# Rust 部署与使用手册

按“部署 → 策略、凭据、agent 准备 → 本机网关/TLS 演示 → DSH 网关/TLS 演示 → 真实云服务验收”操作。第一次可以只做到本机演示，不需要云账号。CensorPrivate 是 AgentCensor 的模型数据隐私子项目。

源码目录使用 `./cpctl`。安装后可在任意目录使用 `~/.local/bin/cpctl`；若已将 `~/.local/bin` 加入 PATH，直接使用 `cpctl`。脚本保存配置，不依赖上一终端的 `CP_BIN`、`CP_RUST_SRC` 等变量。底层长命令见 [Rust 构建教程](build-and-deploy.md)。

## 1. 检查、安装依赖和部署

在仓库根目录执行：

```bash
./cpctl doctor --tls --demo
./cpctl deps --tls --demo --dry-run
./cpctl deps --tls --demo
./cpctl install --tls
```

`doctor` 只检查并列出缺失项，首次失败时先补依赖，再重新检查。`deps` 为 Debian/Ubuntu、openEuler/RHEL 系安装系统软件包，自动选择 apt 或 dnf，需要时使用 sudo。`--dry-run` 只显示安装命令。Rust 工具链需要 ≥1.85，按 [官方说明](https://www.rust-lang.org/tools/install) 预先安装；发行版提供的 Node 版本也需满足下表，不能仅以包安装成功判断版本合格。

| 用途 | 依赖 |
| --- | --- |
| 从源码构建 | Rust ≥1.85、C 链接器、make、pkg-config |
| 运维命令 | Linux、Bash、curl、jq、openssl、coreutils、flock |
| TLS 演示 | OpenSSL 开发库、`/usr/bin/curl` 使用动态 OpenSSL |
| 本机演示、DSH 插件 | Node `^22.19` 或 `>=24`；DSH 演示还需已安装或已构建的 DSH |
| 可选强制隔离 | `doctor/deps --runtime` 检查/安装 iproute2、nftables、unshare、libseccomp；实际启动还需 root 和内核能力 |

普通网关不需要 OpenSSL 开发库，省略 `--tls` 即可。Node 用于演示与 DSH 适配，Rust 产品运行不依赖 Python 或 Node。

源码目录的 `doctor` 检查构建工具链；安装后的 `cpctl doctor` 只检查运行所需依赖，不要求安装 Rust 编译器。

`install` 构建五个 Rust 命令，安装 `cpctl`、可选 native 库、运维脚本、插件和文档，默认前缀为 `~/.local`。公共安装使用：

```bash
./cpctl install --tls --prefix /usr/local
```

构建以当前账户进行，仅向受保护安装目录复制时提权。`bash scripts/package.sh --with-native` 生成的发布归档也包含这些运维文件；从预编译归档安装后无需再运行源码构建命令。

## 2. 前期准备：策略、凭据和 agent

### 2.1 本机演示的准备内容

后文四个 `demo` 命令都会自动准备以下内容，执行结束清理临时进程和目录；失败时保留 0700 诊断目录供本机排查。

| 项目 | 自动准备的内容 |
| --- | --- |
| 策略 | CUSTOMER 词典包含 `DemoCustomerOrchid`；SECRET 词典包含 `DEMO_SECRET_DO_NOT_SEND`；启用金额和日期识别 |
| 输入 | 虚构客户、`100.00元`、`200.00元` 和虚构 SECRET |
| 凭据 | 随机本机 gateway token，不需要 provider key |
| 上游 | 独立回环端口上的确定性模拟模型；TLS 演示另生成短期证书并校验证书 |
| agent | 本机直接演示由脚本发请求；DSH 演示启动真实 headless agent 并注册 `private_compute` |
| 目录 | 配置与 agent workspace 分开；DSH 使用独立 DSH_HOME/profile |

模拟上游只回显或按固定规则发起工具调用。它用于观察实际出网内容，不能代表真实模型推理效果。

### 2.2 实际服务的策略和凭据

```bash
./cpctl init
```

按提示填写 `gateway` 或 `tls`、真实上游 **origin**、服务商提供的实际模型 ID 和 provider key。origin 不包含 `/v1`、其他路径、查询参数或凭据；例如服务商文档中的 API base URL 以 `/v1` 结尾时，只填其 origin。`.example` 是文档占位域名，不能使用。

provider key 无回显输入。若已有 0600 凭据文件，可使用 `--key-file /绝对路径/upstream.key`；它需要属于当前用户。无认证的本机开发上游可显式使用 `--no-key`。不要将 key 放入命令行、仓库或 agent workspace。

默认目录：`~/.config/censorprivacy/managed`，权限 0700；内部文件为 0600：

| 文件 | 用途 |
| --- | --- |
| `config.json` | 模式、origin、模型、端口、凭据是否启用、native 库路径 |
| `policy.yaml` | 从仓库示例复制的策略，需按业务修改 |
| `gateway.token` / `curl.headers` | 本机客户端认证 |
| `upstream.key` | 可选 provider key |
| `run.json` | 启动后生成的本次 run ID；原值映射只在网关内存中 |
| `service.log` | 服务诊断，使用固定错误码 |

先编辑 `policy.yaml`，加入需要保护的业务客户与秘密。系统只能按已配置的规则识别，示例词典不会自动覆盖业务数据。重复 `init` 保留已有配置和凭据；独立部署使用通用选项 `--config-dir /绝对路径/私有目录`。它不会自动导入旧教程的 `~/.config/censorprivacy/rust` 配置。

```bash
./cpctl start
./cpctl check
./cpctl check --model
```

默认网关为 `http://127.0.0.1:8775`。`check` 检查版本、目录/文件权限、认证、run ID；它不调用上游。`check --model` 额外通过网关发送无敏感数据的 Chat Completions 请求，会使用真实上游并可能计费。TLS 模式的实际模型检查使用后文 `dsh cloud`。

`/ready` 成功和 `/v1/models` 返回配置模型，只能说明本机控制面可用。模型请求 502 时检查实际 origin、模型 ID、凭据和网络；具体错误保存在私有 `probe-response.json`。`.example` 或任意自造模型名不会变成可用云服务。

### 2.3 agent 配置

普通 agent 使用 `http://127.0.0.1:8775/v1` 作为 Chat Completions base URL，API key 使用本机 `gateway.token`，模型使用 `config.json` 中的模型 ID。网关模式由 Rust 服务持有 provider key。DSH 的配置由后文 `dsh setup` 自动生成，无需手写环境变量或修改 harness。

TLS direct 的应用保留真实 HTTPS origin，通过 `tls-run` 加载 hook；客户端需要其上游认证凭据。它当前只支持固定 IPv4 peer、HTTP/1.1、Content-Length、非流式未压缩 JSON，证书必须匹配该 IP。不能把域名简单换成解析出来的 IP 后关闭证书校验。DSH 插件替换传输以满足这些要求。

### 2.4 长期托管与停止

本用户实例停止：

```bash
./cpctl stop
```

即使连接配置已损坏，`stop` 仍会核对原进程身份后停止实例，无需先修复上游配置。

系统实例先安装到 `/usr/local`，再执行：

```bash
./cpctl systemd install --dry-run
./cpctl systemd install
./cpctl systemd start
./cpctl systemd check
./cpctl systemd stop
```

系统服务使用 `censorprivacy` 专用账户，unit 名按配置目录生成，避免不同实例同名。`start` 会停止同配置的用户实例，启动后自动检查；不会自动设为开机启动。`systemd check` 将验证后的本次 run 文件同步到私有配置目录，供 DSH/TLS 控制面使用。两种托管方式共用端口，选择一种运行。

`systemd install --dry-run` 将 unit 正文写入标准输出，说明写入标准错误，可重定向保存后用 `systemd-analyze verify` 检查。

修改配置、策略或凭据后，停止并重新启动；系统服务需要停止、重新 `systemd install` 后启动。每次重启产生新 run，旧引用失效，agent 应开启新会话。默认不自动重启，避免让已有会话悄然使用失效引用。首次迁移到系统服务也需重新准备 agent。

## 3. 本机网关演示

```bash
./cpctl demo gateway
```

链路：演示脚本 → Rust 网关 → 本机模拟上游 → Rust 网关 → 本机显示。命令会展示并断言：

1. 输入包含虚构客户、两笔金额和 SECRET。
2. 上游实际收到 `<CP_CUSTOMER_…>`、`<CP_MONEY_…>`、`<CP_SECRET_…>`，没有这些原值。
3. 被动显示恢复客户和金额，SECRET 显示为 `[REDACTED_SECRET]`。
4. `private_compute sum` 接收两个引用，先返回派生金额引用；最终本机显示 `300.00`。
5. 无认证返回 401；带未授权业务数值的请求被拒绝且没有到达上游。

普通网关只保护发到这个入口的流量。需要封闭 agent 绕过网关的网络出口时使用 [privacy-run](../README.md#linux-受管启动)，并满足其 root/namespace 等要求；这些演示没有建立强制隔离域。

## 4. 本机 TLS direct 演示

```bash
./cpctl demo tls
```

链路：动态 OpenSSL curl → hook 与 Rust broker 批准 → 原 HTTPS 连接 → hook 与 broker 批准响应 → 本机显示。演示使用本机测试 CA，不使用 `-k`。

检查点与网关相同。上游看到的是与原始字节等长的短别名，例如 `~P…` 或 `~…`；SECRET 可能显示为星号。派生 CP 引用恢复后可能保留填充空格，这是维持 wire 长度的要求。输出还包含 broker 的请求保护、响应恢复和拒绝计数。

不支持的协议、长度或响应会被拒绝，不会回退成原文直连。`tls-run` 是协作式接入，不替代强制进程隔离。

## 5. DSH 插件适配演示

DSH 即 [DeepSeek Harness，官方运行说明](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.zh.md#run)。先按官方说明安装 DSH；源码方式先执行官方 `pnpm install`、`pnpm run build`。本项目不修改 harness 源码，不向已有 profile 安装插件，也不自动下载或升级 DSH。

已于 2026-10-16 联网核对 [官方快速入门](https://deepseek-harness.github.io/deepseek-harness/en/guide/quickstart#configure-a-model)：其模型配置步骤明确要求 “Open Settings → Models, enter a DeepSeek API key, and save it.”。[官方模型配置页](https://deepseek-harness.github.io/deepseek-harness/en/guide/providers#configure-deepseek) 说明凭据保存在 `$DSH_HOME/.credentials.yaml`，界面只返回脱敏描述；该页目前也说明 OAuth 登录的提供方尚不支持。

不需要真实 provider key 的是启动 DSH 界面和后文的本机模拟演示；调用 DeepSeek 官方云模型需要有效 key。官方适配器默认 origin 是 `https://api.deepseek.com`，模型应从实际可用目录选择；安装包中的建议目录不能证明某个账户可调用该模型。无凭据访问官方 `GET /models` 的本次只读检查返回 401，未发起模型推理。

`cpctl` 使用独立 DSH_HOME，不自动导入已有 DSH 的凭据文件。接入本项目时，用前文 `./cpctl init` 无回显输入 key，或指定私有 key 文件路径即可；无需将密钥内容发给他人。DSH 模拟演示仍然无需这些云端凭据。

源码方式检查和演示：

```bash
./cpctl doctor --tls --demo --dsh --dsh-root /path/to/deepseek-harness
./cpctl demo dsh-gateway --dsh-root /path/to/deepseek-harness
./cpctl demo dsh-tls --dsh-root /path/to/deepseek-harness
```

若 PATH 中已有可用的 `dsh` 命令，省略 `--dsh-root`。仓库脚本直接使用已构建 CLI；不会重新构建 DSH。

两种演示都运行真实 headless agent，通过 Cordis 插件注册 `censorprivacy` 模型路由与精确命名的 `private_compute` 工具。agent 请求模型发起求和，工具经本机认证计算，模型再将派生引用放入最终回复。验收同时检查模拟上游无原值、工具往返记录和最终客户名/`300.00` 显示。

插件使用完整 JSON 请求并适配为 DSH 的块事件；不把 DSH 默认 SSE 传输直接套进 TLS hook。独立 profile 关闭自带模型适配器、辅助模型调用和对应遥测插件，限制工具为 `private_compute`。这些配置用于演示，不能代替 OS 级隔离。图片、opaque reasoning、未知内容块或不支持的输出会拒绝，模型应支持非 reasoning 的 Chat Completions 与工具调用。

实际 agent 的准备与使用：

```bash
./cpctl dsh setup --dsh-root /path/to/deepseek-harness
./cpctl dsh run '请介绍当前可用的 private_compute 工具。'
```

任务以 `-` 开头或包含命令行选项时，用 `--` 分隔，例如 `./cpctl dsh run -- '--help 是任务的一部分，请原样解释。'`。配置选项放在 `--` 之前，后面的内容全部作为任务文本。

先启动当前网关并通过 `check`；系统实例先通过 `systemd check`。`setup` 在私有配置目录的 `dsh/` 下创建隔离 profile、patch 和 workspace，保存 DSH 源码路径；后续无需重复提供。`run` 使用实际上游，会产生真实模型调用。每次命令运行新会话；策略和 run 改动后重新执行 `setup`。

插件源码、标准 bundle 安装方式和配置字段见 [插件说明](../plugins/dsh-censorprivacy/README.md)。模型返回的代码、链接与普通工具参数不获得显示恢复权限。

## 6. DSH 真实云服务效果验收

前提是四个本机演示通过、实际上游和凭据有效、当前服务已启动、DSH 可用。执行：

```bash
./cpctl dsh cloud
```

命令会发起真实模型请求并可能计费。它建立临时独立 Rust run，使用相同 origin、模型和凭据，配上虚构验收策略；不会修改生产策略或业务映射。它要求模型执行 `private_compute sum`，检查引用输入、引用结果、授权客户名与 `300.00` 显示，并确保最终回复不含虚构 SECRET。

结果写入 `dsh/cloud-report.json`（0600），包括模式、模型、origin、run ID、时间、工具执行记录、回复哈希及通过/失败状态；不会记录 key。失败时另保留私有临时诊断。它不会把回环模拟上游计为云验收，模型没有正确调用工具时也不会只因 HTTP 200 而通过。

网关模式支持域名 HTTPS。TLS direct 的云端还需支持匹配证书的固定 IPv4 origin 及前述 HTTP 限制；很多公有模型接口不满足这些条件，应按实际服务测试。任一约束不满足时拒绝，不降级保护。

TLS 云端验收沿用 `init` 保存的 native 库路径；自定义安装位置通过 `init --native-library /绝对路径/libcp_tls_guard.so` 配置。

真实云检查验证本机插件、工具调用和回复效果；出网正文无原值的直接观测由本机模拟验收提供，不能据此声称观测到了云服务内部存储。没有有效配置或无法访问上游时，真实云验收应保持未完成。

## 7. 维护与回归

```bash
./cpctl help
bash scripts/test/operations.sh
bash scripts/test/operations.sh --dsh-root /path/to/deepseek-harness
```

入口 `cpctl` 只定位安装位置；环境检查、部署和演示分别在 `scripts/ops/`，DSH 适配在 `plugins/dsh-censorprivacy/`。配置按 JSON/YAML 读取，不执行 shell 配置。配置目录加锁，进程停止核对 PID、启动时间与可执行文件；凭据和输出拒绝叶子符号链接。

运维回归覆盖安装后的目录布局、初始化保留、凭据路由、进程启停、认证、失效 PID、坏连接配置下停止、文件权限、符号链接、网关/TLS 演示，以及插件拒绝未知内容、原值计算结果和取消时的进程清理。指定 `--dsh-root` 后还验证安装产物的 `dsh setup/run`、任务选项分隔和两种 DSH 演示，不将外部 DSH 作为 Rust 产品编译依赖。

验收环境、核心回归与实际通过项见 [验收记录](validation.md)，模块职责见 [架构说明](architecture.md)。正式系统服务和真实云请求的状态分别记录，不能用本机模拟结果替代。
