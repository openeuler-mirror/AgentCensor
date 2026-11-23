# 统一配置

`CensorPrivate/config.yaml` 同时管理部署参数和检测规则。`server` 管理网关运行参数及有效期，`detection` 选择检测类别和敏感路径，`custom`（自定义）添加业务词典、正则与标签，并通过 epoch 标记规则版本，无需另一个策略文件。模型目录及其中的 `config.json`、`manifest.json` 等文件继续独立保留；网关和上游的认证凭据通过文件路径引用。

源码目录 `~/AgentCensor/CensorPrivate/` 的 [config.yaml](../config.yaml) 已完整填写当前配置：上游 `https://api.deepseek.com`、模型 `deepseek-flash`、监听 `127.0.0.1:8775`、有效期 86400 秒、上游请求超时 360 秒、CPU 线程数 4、batch 1、每窗口 16 秒，检测类别全部开启。模型和运行库使用仓库内的现有路径，凭据和运行状态引用当前 managed 实例目录。未启用的配置显式填写 `null`、`[]` 或 `{}`。迁移机器或制作镜像时，按实际布局修改这些路径和连接参数。版本仍为 0.3.2。

## 启动与校验

已有 `censorprivacy init` 实例时：

```bash
cd ~/AgentCensor/CensorPrivate
# 自动读取源码根目录的 config.yaml
./censorprivacy start
# 或指定部署文件
./censorprivacy start --config /etc/censorprivacy/config.yaml
```

`censorprivacy` 使用实例 `config.json` 的连接、端口和凭据，并将其作为命令行参数传递；这些值优先于 YAML 的对应字段。业务策略、类别、TTL、NER、资源路径和请求限制统一读取 YAML。`--config-dir` 仍表示实例状态目录，`--config` 表示新的部署文件。容器中直接运行二进制，可完全通过 YAML 配置连接：

```bash
./target/release/censorprivacy config-check --config ./config.yaml
./target/release/censorprivacy serve --config ./config.yaml
```

`config-check` 检查 YAML 格式、未知字段、数值范围、类别以及 `custom` 的字典、正则、标签和 epoch；不加载模型、不读取凭据、不验证上游连通性，允许校验只含部分参数的配置。成功输出 `{"status":"valid"}`，失败返回非零状态和固定错误码。`--print-resolved` 输出路径已解析的 JSON（同时是合法 YAML），可用于检查路径。输出包含业务字典和规则，请按业务策略文件管理。

配置文件选择顺序：`--config` → `CP_CONFIG_FILE` → 当前工作目录的 `config.yaml`。`censorprivacy start/systemd` 的自动查找位置为源码根目录或安装后的 `share/censorprivacy/config.yaml`，与执行时的目录无关。显式指定的文件缺失会报错。`--no-config` 禁用统一配置和环境变量发现，使用 CLI 与内置策略。兼容旧实例时，`censorprivacy start --no-config` 仍可读取已有的实例 `policy.yaml`；新初始化不再生成该文件。正常使用统一配置时，旧文件不参与读取，启动会提示迁移其中的自定义字典和规则，旧文件不会被自动删除。

参数优先级为显式 CLI → YAML → 已有环境变量 → 内置默认值。`censorprivacy` 的 NER 优先级为 `start --ner-*` → YAML → 实例 `config.json` → 默认值；临时覆盖不会改写实例配置。相对文件路径基于 YAML 所在目录，`~` 和 `$VAR` 不进行 shell 展开，请使用绝对路径或相对路径。

## 类别选择

随附的 `config.yaml` 明确设置 `detection.categories: all`，启用全部内置类别（包括普通数字 NUMBER）和 `custom` 注册的类别。省略此项（包括省略整个 `detection` 段），或写成 `null`、`[]`，均关闭按类别检测。已有配置若用省略或 `null` 表示全开，请改为 `all`。

```yaml
# 只检测邮箱、电话、身份证/证件号和秘密
detection:
  categories: [EMAIL, PHONE, ID, SECRET]
```

配置列表后仅运行列表中的检测类别，作用于正则、规则、NER、`custom` 字典/正则和外部检测结果。类别名称区分大小写；全开时使用小写的 `all`，写在列表外。未知类别在启动前报错，避免拼写错误导致静默漏检。

| 类别 | 内容 |
| --- | --- |
| PERSON、ORG、LOCATION | 姓名、组织、地点 |
| ADDRESS | 通讯地址 |
| EMAIL、PHONE | 邮箱、电话 |
| ID、ACCOUNT | 证件号、银行卡/账号 |
| SECRET | 密码、密钥、令牌 |
| MONEY、DATE、NUMBER | 金额、日期时间、普通数字 |
| IP_ADDRESS、MAC_ADDRESS | IP、MAC 地址 |
| LICENSE_PLATE | 车牌 |
| CONFIDENTIAL | 自定义机密规则及部分保守保护结果 |

接受别名：PER→PERSON、ORGANIZATION→ORG、LOC→LOCATION、EMAIL_ADDRESS→EMAIL、PHONE_NUMBER→PHONE、CHINA_ID/ID_NUMBER/US_SSN→ID、CREDIT_CARD→ACCOUNT、PASSWORD→SECRET、DATE_TIME/TIME→DATE。

自定义类别直接在同一配置的 `custom.dictionaries` 或 `custom.patterns` 中定义：

```yaml
server:
  ttl_seconds: 86400
custom:
  epoch: 1
  dictionaries:
    CUSTOMER: [示例客户甲, 示例客户乙]
  patterns:
    PROJECT: ['PROJECT-[A-Z]+']
  default_labels: [confidential]
detection:
  categories: [EMAIL, CUSTOMER, PROJECT]
```

迁移旧配置时，将 `policy` 改名为 `custom`，仅保留 `epoch`、`dictionaries`、`patterns`、`default_labels`；把 `ttl_seconds` 移到 `server` 下，删除 `mask_money`、`mask_dates`、`mask_all_numbers`，使用 `detection.categories` 选择类别。旧 `policy` 键、`custom.ttl_seconds`、三个 `mask_*` 开关以及 `file` 路径均不再是统一配置的合法字段。旧独立策略文件也按此拆分迁移，检查通过并重启后可归档或删除旧文件。

类别开关统一由 `detection.categories` 控制；`custom` 只补充规则，不负责启用类别。启用 PERSON 后，为 PERSON 添加词条会增强姓名检测。新增 CUSTOMER 类别时，`all` 会包含它；使用列表则需要显式加入 CUSTOMER。启用 NUMBER 时，文本数字会被保护，业务 JSON 中的私密数值仍按既有协议要求拒绝直接发送；合法协议控制参数继续使用原有校验，例如 `max_tokens`。类别选择不保证某类别的任意写法都能识别，检测范围仍受已有模型与规则限制。

关闭按类别检测后，未标记、未注册的普通文本直接转发；认证、TTL、协议校验、请求限制、显式敏感路径、已注册敏感值和引用授权仍然生效。

`sensitive_paths` 显式标记的整字段保护、已经注册到本次 run 的引用授权独立于列表。普通敏感原文只在所属会话内传播，金额、日期和普通数值需要当前文本的检测依据，不因历史出现相同数字而自动遮蔽。SECRET 和带 secret 标签的凭据继续在本次 run 内保护，网关自身凭据也不受会话切换影响。金额的某些表达（例如带“万”的数值）会作为 CONFIDENTIAL 不透明值保护，避免将不支持的金额格式误用于私密计算；其检测开关仍是 MONEY。

HTTP 网关按本机 Agent/会话标识区分普通敏感值，此行为不依赖是否启用会话记录；无会话标识的请求仅在当前请求内传播。DSH 插件自动携带标识，标识不发送给上游。会话用于控制传播范围，不构成独立用户认证或新的引用访问权限边界。更新后需重启网关与 Agent 并新建会话，旧引用不能跨网关重启使用。

## 可配置参数

所有字段均可省略；未知字段和重复字段会报错。

| 字段 | 默认或限制 |
| --- | --- |
| `server.host` / `port` | 直接运行默认 `127.0.0.1:8765`；端口 1–65535 |
| `server.upstream` | 上游 origin，例如 `https://api.deepseek.com`；启动必填 |
| `server.models` | 允许的模型 ID 列表；启动必填且非空 |
| `server.token_file` | 网关令牌文件；启动必填 |
| `server.upstream_key_file` | 可选上游 Key 文件 |
| `server.upstream_ca_file` | 可选上游 CA 文件 |
| `server.tls_cert` / `tls_key` | 可选 HTTPS 证书与私钥，须成对配置 |
| `server.tls_direct` | 默认 false，沿用 TLS broker 模式 |
| `server.allow_http_test_upstream` | 默认 false，仅用于受信任本机 HTTP 测试上游 |
| `server.run_state_file` | 可选 run ID 状态文件，父目录须为 0700 |
| `server.ttl_seconds` | run 有效期；随附配置 86400 秒，省略时内置默认 3600；大于 0、至多 315360000 秒，可用小数 |
| `detection.categories` | 随附配置为 `all`，开启全部类别；列表仅启用所列项；省略、`null` 或 `[]` 关闭按类别检测 |
| `detection.sensitive_paths` | 可选 JSON Pointer 前缀列表，例如 `[/metadata/customer]` |
| `ner.threads` | 默认 1；1–256，支持原有 `CP_NER_THREADS` |
| `ner.device` | 默认 cpu；`cuda[:ids]`、`cann[:ids]`；支持原有 `CP_NER_DEVICE` |
| `ner.batch_size` | 默认 1；1–256，按每卡计算；支持原有 `CP_NER_BATCH_SIZE` |
| `ner.window_timeout_seconds` | 默认 16；1–3600 秒，每批按实际窗口数乘算，无整篇固定总额 |
| `ner.model_dir` / `ort_library` | 本地 ONNX 模型与 ORT 文件；优先于原有资源环境变量 |
| `custom.epoch` | 规则版本，默认 1，正整数；热重载时必须递增 |
| `custom.dictionaries` | 类别到精确匹配词条列表的映射；默认空，词条不得为空 |
| `custom.patterns` | 类别到正则列表的映射；默认空，不接受匹配空字符串的正则 |
| `custom.default_labels` | 默认 `[confidential]`；非空标签集合 |
| `limits.max_body_bytes` | 默认 8388608；1–268435456 字节 |
| `limits.max_decoded_bytes` | 默认 33554432；1–268435456 字节 |
| `limits.upstream_timeout_seconds` | 随附配置 360，省略时内置默认 60；1–86400 秒，上游请求超时，与本地推理超时独立 |
| `private_tools_file` | 可选私密程序注册 JSON；保留现有仅回环监听限制 |
| `conversation_records_dir` | 可选会话记录目录；保留现有 IPv4 本机 HTTP 监听限制 |

`max_body_bytes` 限制网关接收的 HTTP 请求体，`max_decoded_bytes` 限制解压后的请求体及读取的上游响应。两者以字节计量，与模型上下文或单次生成的 token 上限独立。

TTL 从 run 创建时计算，持续请求不会续期。修改配置后通常执行 `./censorprivacy stop`、`./censorprivacy start` 并创建新的 Agent 会话。

需要热重载自定义规则或 TTL 时，增加 `custom.epoch` 后向网关发送 SIGHUP（systemd 使用 `systemctl reload`）。进程重新读取同一 `config.yaml` 的 `custom` 和 `server.ttl_seconds`；类别、NER、监听、凭据及其他部署设置仍使用启动时的值，更改这些参数需重启。重载会撤销旧 epoch 的引用与许可，不能延长现有 run 的有效期；无效配置或未递增 epoch 会使当前服务拒绝后续请求，应修正后重启或以更高 epoch 重载。

`censorprivacy dsh cloud` 是使用虚构业务策略的独立验收，继续继承当前网关的生效 NER 线程、设备和 batch；演示与 cloud 验收不加载部署文件的类别选择。`dsh run/web` 和 xiaoO 日常请求通过当前网关，使用其部署设置。

独立 `censorprivacy web --config ...` 使用 YAML 的 NER 和类别设置，页面继续只在本机开放，默认端口 8766，使用 `web --port` 修改；`server.host/port` 专用于网关。页面的文本检测预览显示原生检测器结果；`custom` 自定义规则的实际保护结果以网关为准。

`censorprivacy systemd install --config ...` 将解析相对路径后的部署快照安装到 `/etc/censorprivacy/<实例>.config.yaml`，其中包含内嵌策略，不再安装独立 policy 文件。修改原文件后需重新执行安装再启动服务；systemd 的热重载读取安装后的快照。首次源码安装生成配置时会清空与本机有关的凭据、模型、运行库及记录路径，以便在安装位置重新配置；重新安装保留已经存在的统一配置。模型和运行库为 `null` 时使用现有环境变量。完整交付包在构建时填入包内相对模型、运行库路径，移动目录仍然有效。模型、ORT 及其他引用文件须位于服务账号可读的位置；默认 systemd 的 `ProtectHome` 会阻止访问 `/root`、`/home`。

## Kubernetes 挂载

ConfigMap 只需投射一个含内嵌策略的 `config.yaml`，加载器支持投射卷的符号链接。容器使用下面的配置，再以 `censorprivacy serve --config /etc/censorprivacy/config.yaml` 启动：

```yaml
server:
  host: 0.0.0.0
  port: 8775
  upstream: https://api.deepseek.com
  models: [deepseek-chat]
  token_file: /run/cp-private/gateway.token
  upstream_key_file: /run/cp-private/upstream.key
  ttl_seconds: 86400
detection:
  categories: all
ner:
  threads: 1
  model_dir: /opt/censorprivacy/share/censorprivacy/models/bert4ner-base-chinese
  ort_library: /opt/censorprivacy/lib/censorprivacy/libonnxruntime.so
custom:
  epoch: 1
  dictionaries: {}
  patterns: {}
  default_labels: [confidential]
```

配置与凭据分别创建，文件名保持与上述路径一致：

```bash
kubectl create configmap censorprivacy-config --from-file=config.yaml=./config.yaml
kubectl create secret generic censorprivacy-credentials \
  --from-file=gateway.token=/本机私有目录/gateway.token \
  --from-file=upstream.key=/本机私有目录/upstream.key
```

凭据读取保留原有安全要求：必须为普通文件、禁止最终路径符号链接、权限不得开放给组/其他用户。Kubernetes Secret 投射文件也是符号链接，须通过 initContainer 复制到 `emptyDir` 并设为 0600（目录 0700），再以只读方式挂载到应用容器的 `/run/cp-private`。initContainer 与应用使用同一个 UID，复制示例：

```sh
umask 077
chmod 700 /run/cp-private
cp /projected-secret/gateway.token /run/cp-private/gateway.token
cp /projected-secret/upstream.key /run/cp-private/upstream.key
chmod 600 /run/cp-private/gateway.token /run/cp-private/upstream.key
```

镜像需包含二进制、模型和 ORT，路径按镜像布局调整；本次没有构建或发布镜像。外部 TLS 可由 Ingress/Sidecar 终止。健康检查 `/ready` 需要网关 Bearer Token，并会检查 run 是否过期；ConfigMap/Secret 更新后应滚动重启 Pod，以加载新配置和凭据。每个 Pod 持有独立的内存 Vault 与引用，多个副本需要会话粘滞或保持单副本，跨 Pod 不能复用引用。
