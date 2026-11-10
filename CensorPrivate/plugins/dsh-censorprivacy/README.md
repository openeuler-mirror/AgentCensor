# DSH CensorPrivacy 插件

通过 Cordis 注册 `censorprivacy` 模型路由与 `private_compute` 工具。网关与 TLS direct 共用完整 JSON 适配器，不修改 DSH 源码。TLS 使用 `tls-run` 启动动态 OpenSSL curl；不支持的媒体、reasoning 和协议格式拒绝处理。

推荐从仓库或交付包执行 `./cpctl dsh web --dsh-root /path/to/deepseek-harness` 启动浏览器聊天界面；命令行任务使用 `./cpctl dsh setup` 和 `./cpctl dsh run '任务'`。也可用官方 `dsh plugin --profile 隔离profile add /绝对路径/plugins/dsh-censorprivacy` 安装本地 bundle。插件没有 npm 运行依赖或构建脚本。

`cpctl` 使用独立 DSH_HOME，Web 和 headless 分别位于私有配置目录的 `dsh-web/` 与 `dsh/`，经绝对模块路径加载同一插件；它不会修改现有 DSH profile。Web 默认监听本机 3080，`--port` 可指定端口；终端打印带 token 的完整网址。Web 自动复用已启动网关和凭据，当前只开放 `private_compute`，输出预算为 4096 token。按 Ctrl+C 只停止 Web；网关重启后需重启 Web 并新建会话。标准 bundle 的 `dsh.bundle.patch` 是另一种集成方式，需由集成者准备隔离 profile 和配置。

| 配置 | 含义 |
| --- | --- |
| `mode` | `gateway` 或 `tls` |
| `baseURL` / `model` | 网关本机 origin 或 TLS 上游 IPv4 origin；实际模型 ID |
| `broker` | 必须为 `http://127.0.0.1:端口` |
| `tlsRun` / `nativeLibrary` | TLS launcher 和 native 库绝对路径 |
| `tokenFile` / `runStateFile` | TLS 控制面私有文件；gateway token 从 `CENSORPRIVACY_TOKEN` 环境读取 |
| `upstreamKeyFile` / `caFile` | TLS 客户端可选私有凭据和自定义 CA |
| `workspace` | TLS 子进程工作目录，和控制面文件分离 |
| `timeoutMs` / `maxTokens` / `contextWindow` | 默认 90000 / 512 / 32768；传输有总时间、请求和响应尺寸限制 |
| `privateToolsOnly` | 演示设为 true；按 agent 限制工具，并在执行时拒绝其他工具 |
| `receiptFile` | 可选 0600 本机验收记录，只记事件与数量，不记原值、key 或引用内容 |

支持文本、原生工具调用与工具结果。图片、未知块、非空 reasoning、拒绝通道和无法映射的输出会拒绝；不自动重试或降级成其他传输。私密工具只接受引用，并复验返回对象的所有字段，额外原值字段也会拒绝。

对 deepseek-* 模型，插件显式发送 thinking: {"type":"disabled"}，支持 deepseek-flash 等默认开启思考的模型使用普通文本与工具调用。其他模型不添加该参数；若上游仍返回非空 reasoning_content，插件继续拒绝该响应。

非流式 tool_calls 的可选 index 必须为非负安全整数，仅作元数据校验，不传入工具参数。private_compute 的 schema 和说明要求完整复制引用（含尖括号），缺少分隔符仍会拒绝。本机网关错误仅显示允许列表中的固定原因码，不输出请求或上游错误正文。

`privateToolsOnly` 同时限制执行权限和发给模型的工具目录，过滤 Harness 后续注入的 agent 局部工具。`date_diff` 必须提供两个日期引用，按第二个日期减第一个日期计算天数；不支持 `reference: today` 或隐含当前日期。数值运算不接受 options，`aggregate` 可指定 operations，日期差只接受 unit=days。无效参数以固定工具错误返回，模型可以更正调用或向用户补问；适配器不会填造引用或放宽网关保护。

部署与两种演示见 [Rust 使用手册](../../docs/usage.md)。provider key 不写进 bundle 或 profile；TLS 客户端通过私有文件读取，经 stdin 交给 curl。已验收 DSH `0.1.5-rc.2` / Node `22.22.3`，其他版本按相同流程复验。

[DSH 官方快速入门](https://deepseek-harness.github.io/deepseek-harness/en/guide/quickstart#configure-a-model) 要求在 Settings → Models 配置 DeepSeek API key。无需真实 key 的是本项目的模拟演示；实际官方模型请求需要有效凭据。`cpctl` 不自动导入已有 DSH_HOME 的 `.credentials.yaml`，请通过 `cpctl init` 准备本机私有凭据。
