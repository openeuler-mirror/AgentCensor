# DSH CensorPrivacy 插件

通过 Cordis 注册 `censorprivacy` 模型路由与私密工具（内置 `private_compute` 和本机配置的自定义工具）。gateway 使用真实 Chat Completions SSE，逐段映射为 DSH 事件；TLS direct 保留完整 JSON 响应，使用 `censorprivacy tls-run` 启动动态 OpenSSL curl。不修改 DSH 源码；隐私检测、秘密留尾和引用恢复由共享网关处理。

兼容性验证基线为 DSH **`0.2.2-rc.1`**（`dsh-v0.2.2-rc.1`，提交 `66491b05e3f05ad35bcdd4f6cd7ed26041d9e3db`）。插件与 `censorprivacy` 不限制 DSH 版本；更换版本后应复验插件加载、模型事件、工具往返及 CLI/Web 显示。准备步骤见 [使用手册](../../docs/usage.md#32-deepseek-harness浏览器聊天)。

推荐从仓库或交付包执行 `./censorprivacy dsh web --dsh-path /path/to/deepseek-harness` 启动浏览器聊天界面；命令行任务使用 `./censorprivacy dsh setup` 和 `./censorprivacy dsh run '任务'`。也可用官方 `dsh plugin --profile 隔离profile add /绝对路径/plugins/dsh-censorprivacy` 安装本地 bundle。插件没有 npm 运行依赖或构建脚本。

`censorprivacy` 使用独立 DSH_HOME，Web 和 headless 分别位于私有配置目录的 `dsh-web/` 与 `dsh/`，经绝对模块路径加载同一插件；它不会修改现有 DSH profile。Web 默认监听本机 3080，`--port` 可指定端口；终端打印带 token 的完整网址。Web 自动复用已启动网关和凭据，输出预算为 4096 token。Web 和 headless 默认只启用私密工具；本次运行要开放 DSH 原生工具时加 `--enable-tools`。按 Ctrl+C 只停止 Web；网关重启后需重启 Web 并新建会话。标准 bundle 的 `dsh.bundle.patch` 是另一种集成方式，需由集成者准备隔离 profile 和配置。

| 配置 | 含义 |
| --- | --- |
| `mode` | `gateway` 或 `tls` |
| `baseURL` / `model` | 网关本机 origin 或 TLS 上游 IPv4 origin；实际模型 ID |
| `broker` | 必须为 `http://127.0.0.1:端口` |
| `executable` / `nativeLibrary` | TLS launcher 和 native 库绝对路径 |
| `tokenFile` / `runStateFile` | TLS 控制面私有文件；gateway token 从 `CENSORPRIVACY_TOKEN` 环境读取 |
| `upstreamKeyFile` / `caFile` | TLS 客户端可选私有凭据和自定义 CA |
| `workspace` | TLS 子进程工作目录，和控制面文件分离 |
| `timeoutMs` / `maxTokens` / `contextWindow` | 默认 90000 / 512 / 32768；传输有总时间、请求和响应尺寸限制 |
| `privateToolsOnly` | 默认 true，限制目录和执行权限；设为 false 时同时开放 DSH 原生工具 |
| `privateTools` | 认证 `GET /v1/private_tools` 返回的 `tools` 数组；`censorprivacy` 自动获取，手动集成可填入插件配置 |
| `conversationSession` | 可选；手动适配器调用没有传入 `sessionId` 时显式指定会话（1–128 位 ASCII 字母、数字、`-_.:`），每会话独立配置 |
| `receiptFile` | 可选 0600 本机验收记录，只记事件与数量，不记原值、key 或引用内容 |

gateway 模式自动将 Harness 的真实 `sessionId` 和 `deepseek-harness` 来源作为本机请求头传给网关；`./censorprivacy dashboard` 显示检测台地址，可查看 CLI/Web 的会话、回复和每条消息的实际遮蔽详情。标题和压缩等辅助调用不进入会话记录；没有会话标识的调用不猜测归组。这些元数据不进入模型请求体，不向上游转发。网关需启用 `--conversation-records-dir`（`censorprivacy start` 默认启用）；更新后需重启网关与 Harness。TLS direct 不记录。

支持文本、原生工具调用与工具结果；DSH 0.2.2 的独立 `tool` 消息和早期嵌套 `tool-result` 均可映射为 Chat Completions 工具结果。插件实现新版图像请求元数据接口，但模型目录仍只声明文本输入；图片、未知块、非空 reasoning、拒绝通道和无法映射的输出会拒绝。私密工具只接受引用，并复验返回对象的所有字段，额外原值字段也会拒绝。

gateway 请求发送 `stream:true`。SSE 按字节增量解码，支持分片 UTF-8、CRLF、注释和多行 data；每帧校验通过后释放文字。工具参数在完整调用及 `[DONE]` 校验通过后才交给 DSH，截断、异常或取消不会触发不完整工具调用。上游仍返回 JSON 时按实际 Content-Type 完整校验，不发起额外请求。请求/响应各限 1 MiB，总超时覆盖整条流，取消或提前停止消费会关闭连接。

`censorprivacy dsh run` 通过私有子进程管道即时转发适配器已经取得的显示文字，避开原生 headless 仅在末尾打印总结的行为；原生总结保留在本机诊断文件，终端不重复打印。中间工具轮次的文字也会显示。Web 继续使用 DSH 原生事件，不使用该管道。`cliStream` 是 censorprivacy 管理的 headless 配置，外部自定义 profile 不应手动启用。TLS direct 仍在完整响应校验后显示文字。

对 deepseek-* 模型，插件显式发送 thinking: {"type":"disabled"}，支持 deepseek-flash 等默认开启思考的模型使用普通文本与工具调用。其他模型不添加该参数；若上游仍返回非空 reasoning_content，插件继续拒绝该响应。

JSON tool_calls 的可选 index 必须为非负安全整数，SSE 使用 index 聚合分片，两者均不将 index 传入工具参数。private_compute 的 schema 和说明要求完整复制引用（含尖括号），缺少分隔符仍会拒绝。本机网关错误仅显示允许列表中的固定原因码，不输出请求或上游错误正文。

`privateToolsOnly` 同时限制执行权限和发给模型的工具目录，过滤 Harness 后续注入的 agent 局部工具。`date_diff` 必须提供两个日期引用，按第二个日期减第一个日期计算天数；不支持 `reference: today` 或隐含当前日期。数值运算不接受 options，`aggregate` 可指定 operations，日期差只接受 unit=days。无效参数以固定工具错误返回，模型可以更正调用或向用户补问；适配器不会填造引用或放宽网关保护。

部署与两种演示见 [Rust 使用手册](../../docs/usage.md)。provider key 不写进 bundle 或 profile；TLS 客户端通过私有文件读取，经 stdin 交给 curl。已验收 DSH `0.2.2-rc.1` / Node `24.21.0`。

[DSH 官方快速入门](https://deepseek-harness.github.io/deepseek-harness/en/guide/quickstart#configure-a-model) 要求在 Settings → Models 配置 DeepSeek API key。无需真实 key 的是本项目的模拟演示；实际官方模型请求需要有效凭据。`censorprivacy` 不自动导入已有 DSH_HOME 的 `.credentials.yaml`，请通过 `censorprivacy init` 准备本机私有凭据。
