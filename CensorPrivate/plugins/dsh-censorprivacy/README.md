# DSH CensorPrivacy 插件

为 DeepSeek Harness（DSH）接入 CensorPrivacy 本机隐私网关，并在顶栏“轨迹”之后新增 **“隐私”** 页，查看系统提示词及各角色输入在遮蔽前后的文本。

[安装与启动](#安装与启动) · [查看隐私对比](#查看隐私对比) · [常用操作](#常用操作) · [配置参考](#配置参考) · [开发与打包](#开发与打包)

## 安装与启动

### 准备条件

- 已准备 CensorPrivacy 可执行程序、检测模型及 ONNX Runtime。完整包内置这些资源；源码部署见 [构建与部署](../../docs/build-and-deploy.md)。
- 已安装或构建 DSH。验证版本为 **`0.2.2-rc.1`**，需要 **Node ≥24.9.0**；安装步骤见 [使用手册](../../docs/usage.md#32-deepseek-harness浏览器聊天)。
- 实际模型请求需要有效的 provider API Key；本项目的模拟演示无需云 Key。

以下两种接入方式任选其一。日常使用推荐方式一，启动器会自动加载插件并准备独立配置。

### 方式一：通过 CensorPrivacy 启动（推荐）

以下命令在 `CensorPrivate/` 源码目录或 CensorPrivacy 完整包目录执行。

**1. 首次使用：初始化模型连接。** 以 DeepSeek 为例，按提示输入 API Key：

```bash
./censorprivacy init --upstream https://api.deepseek.com --model-id deepseek-flash
```

已有配置可跳过此步。CensorPrivacy 不自动导入现有 DSH_HOME 中的 `.credentials.yaml`。

**2. 启动并检查网关。** 网关已经运行时，只需执行检查：

```bash
./censorprivacy start
./censorprivacy check
```

**3. 启动 DSH Web。** 将路径替换为自己的 DSH 目录：

```bash
./censorprivacy dsh web --dsh-path /path/to/deepseek-harness
```

打开终端打印的 **带 token 的完整网址**，选择工作目录，确认模型为 `CensorPrivacy / <已配置模型ID>`，即可聊天。默认端口为 `3080`；首次直接访问不带 token 的地址会返回 401。

此方式自动复用当前网关的模型与凭据，**无需再执行手动插件安装命令**，也不会修改现有 DSH profile。

### 方式二：手动安装到自己的 DSH profile

需要自行管理 DSH profile 时，使用官方插件命令安装本地插件目录：

```bash
dsh plugin --profile 隔离profile add /绝对路径/plugins/dsh-censorprivacy
```

将 `隔离profile` 替换为自己的 profile 名称，例如privacy，将路径替换为插件所在的绝对路径。

该命令完成插件安装；网关连接、模型和认证仍需配置。随包的 [cordis.patch.yml](cordis.patch.yml) 从环境变量读取连接参数，gateway 模式所需的主要变量如下：

| 环境变量 | 填写内容 |
| --- | --- |
| `CP_DSH_BASE_URL` | 本机网关 origin，例如 `http://127.0.0.1:8775`，不加 `/v1` |
| `CP_DSH_MODEL` | 上游实际模型 ID，例如 `deepseek-flash` |
| `CENSORPRIVACY_URL` | 本机私密工具 broker 地址，通常与网关 origin 相同 |
| `CENSORPRIVACY_TOKEN` | 当前网关的认证 token，非 provider API Key |

启动 DSH 时需使用同一 profile，并让进程继承这些变量。其他参数见 [配置参考](#配置参考)。插件包含已构建的浏览器文件，没有 npm 运行依赖。

## 查看隐私对比

1. 在 DSH 中新建会话并发送消息。
2. 点击顶栏“轨迹”之后的 **“隐私”**。
3. 选择模型请求轮次，再选择系统提示、用户输入、助手上下文或工具结果。
4. 对照原文与当次实际发往模型的遮蔽文本。

页面自动同步当前会话的网关记录，标出敏感范围、替换引用和类别数量。原始空白、换行、重复值与 Unicode 文本保持原样；桌面端左右对照，窄屏上下排列，刷新后仍可回看。

**记录范围：** 对比功能使用 gateway 模式，网关需启用 `--conversation-records-dir`（`censorprivacy start` 默认启用）。未经过网关的历史不会补录，TLS direct 暂不提供对比记录。更新后重启网关与 Harness，并新建会话。

### 数据与访问权限

- 对比数据来自网关已保存的输入快照，不重新检测或计算遮蔽结果，也不额外加载模型。
- 浏览器通过 DSH 已认证的只读接口 `/api/censorprivacy/session` 访问记录，不持有网关 token。原文只在当前组件内存中使用，不写入浏览器存储。
- 页面与 DSH 聊天共用所有者认证；能访问该 DSH 实例的所有者，也能读取其会话隐私记录。
- 没有记录、网关离线和处理中会分别显示对应状态，离线后自动重试。

需要查看 CLI/Web 会话的独立检测台时，运行：

```bash
./censorprivacy dashboard
```

## 常用操作

### 更换端口或关闭 DSH 原生工具

```bash
# 使用其他 Web 端口
./censorprivacy dsh web --dsh-path /path/to/deepseek-harness --port 3081

# 关闭 DSH 原生工具，仅保留私密工具
./censorprivacy dsh web --dsh-path /path/to/deepseek-harness --disable_tool
```

Web 和命令行默认同时开放 DSH 原生工具、内置 `private_compute` 和本机配置的自定义私密工具。`--disable_tool` 仅对本次运行生效，关闭原生工具后仍可使用私密工具。旧参数 `--enable-tools`、`--no-tools` 继续兼容；启用和关闭参数不能同时使用。

按 `Ctrl+C` 只停止 Web，网关继续运行。网关重启后，需重新启动 Web 并新建会话；Web 重启后请使用终端新打印的 token 网址。

### 执行命令行任务

```bash
./censorprivacy dsh setup --dsh-path /path/to/deepseek-harness
./censorprivacy dsh run '请整理以下信息：……'
```

`setup` 准备独立配置并记住 DSH 路径；`run` 执行任务并即时显示模型文字，包括中间工具轮次的文字。命令行模式不加载 Web 桥或 React。

启动器使用独立 `DSH_HOME`：Web 与命令行配置分别位于 CensorPrivacy 私有配置目录的 `dsh-web/` 和 `dsh/`，通过绝对模块路径加载同一插件。

## 配置参考

通过方式一启动时，常用配置由启动器自动生成。以下供手动集成或开发时查阅，字段位于 Cordis 插件的 `config` 中。

### 通用配置

| 配置项 | 含义与默认值 |
| --- | --- |
| `mode` | `gateway` 或 `tls`；隐私对比页需要 `gateway` |
| `baseURL` | gateway 为本机网关 origin；TLS 为上游 IPv4 origin |
| `model` | 上游实际模型 ID |
| `broker` | 私密工具服务，必须为 `http://127.0.0.1:端口` |
| `timeoutMs` | 总超时，默认 `90000` 毫秒 |
| `maxTokens` | 输出预算，插件默认 `512`；CensorPrivacy Web 启动器设为 `4096` |
| `contextWindow` | 上下文窗口，默认 `32768` |
| `privateToolsOnly` | 默认 `false`；设为 `true` 时仅保留私密工具 |
| `privateTools` | 认证 `GET /v1/private_tools` 返回的 `tools` 数组；启动器自动获取，手动集成可自行填入 |
| `conversationSession` | 可选；适配器调用未传 `sessionId` 时显式指定会话。每会话独立配置，限 1–128 位 ASCII 字母、数字或 `-_.:` |
| `receiptFile` | 可选；权限为 `0600` 的本机验收记录，只记事件与数量，不记原值、key 或引用内容 |

gateway token 通过进程环境变量 `CENSORPRIVACY_TOKEN` 传入。provider API Key 不写入插件包或 profile，由网关持有。

### TLS direct 配置

TLS direct 使用 `censorprivacy tls-run` 启动动态 OpenSSL curl，完整校验 JSON 响应后显示文字；**暂不记录会话输入对比**。

| 配置项 | 含义 |
| --- | --- |
| `executable` / `nativeLibrary` | TLS launcher 与 native 库的绝对路径 |
| `tokenFile` / `runStateFile` | TLS 控制面私有文件 |
| `upstreamKeyFile` / `caFile` | 可选的上游凭据文件与自定义 CA |
| `workspace` | TLS 子进程工作目录，与控制面文件分离 |

TLS 客户端从私有文件读取凭据，经 stdin 交给 curl。部署与演示步骤见 [使用手册](../../docs/usage.md#tls-direct)。

## 开发与打包

在 `CensorPrivate/` 下执行，无需安装 npm 依赖：

```bash
bash scripts/build-dsh-plugin.sh
```

该脚本自动构建浏览器文件，并输出 `dist/dsh-censorprivacy-0.3.2.tgz`，包含 Node 插件、只读桥和 `lib/client.js`。

只修改前端、暂不打包时，可单独构建或检查：

```bash
# 修改 src/client.cjs 后重新生成浏览器文件
node plugins/dsh-censorprivacy/scripts/build-client.mjs

# 检查已生成文件是否与源码一致
node plugins/dsh-censorprivacy/scripts/build-client.mjs --check
```

`lib/client.js` 随源码、插件包与完整安装包分发，正常使用无需构建。前端使用 DSH 提供的 React，通过官方 `dsh.client` manifest 和 `conversation.view` slot 注册，无需修改 Harness 源码。

### 兼容性与验证

已验收 **DSH `0.2.2-rc.1` / Node `24.21.0`**。对应 DSH 源码标签为 `dsh-v0.2.2-rc.1`，提交为 `66491b05e3f05ad35bcdd4f6cd7ed26041d9e3db`。

插件与 CensorPrivacy 不限制 DSH 版本。更换版本后，应复验插件加载、模型事件、工具往返及 CLI/Web 显示；测试入口见 [脚本速查](../../docs/scripts.md#4-测试与验收)。

<details>
<summary>适配与传输细节</summary>

插件通过 Cordis 注册 `censorprivacy` 模型路由与私密工具，隐私检测、秘密留尾和引用恢复由共享网关处理。

**会话关联**

- gateway 请求携带 Harness 的真实 `sessionId` 与 `deepseek-harness` 来源；这些元数据用于本机会话归组及普通敏感值的传播隔离，不进入模型请求体，也不向上游转发。金额等数值需要当前文本的检测依据；SECRET 保留网关 run 内的凭据保护。
- Web 桥按网关相同算法将真实 Harness session ID 映射成本机记录 ID，仅返回对应会话、`deepseek-harness` 来源的输入快照。
- 标题、压缩等辅助调用不进入会话记录；无会话标识的调用不猜测归组。

**内容与流式响应**

- 支持文本、原生工具调用与工具结果，兼容 DSH 0.2.2 的独立 `tool` 消息及早期嵌套 `tool-result`。
- 模型目录仅声明文本输入。虽已实现新版图像请求元数据接口，图片、未知块、非空 reasoning、拒绝通道及无法映射的输出仍会被拒绝。
- gateway 使用 `stream:true`，将 Chat Completions SSE 逐段映射为 DSH 事件。解码支持分片 UTF-8、CRLF、注释与多行 data，每帧校验通过后释放文字。
- 工具参数在完整调用及 `[DONE]` 校验通过后才交给 DSH，截断、异常或取消不会触发不完整工具调用。上游返回 JSON 时按实际 Content-Type 完整校验，不额外请求。
- 请求、响应各限 1 MiB，总超时覆盖整条流；取消或提前停止消费会关闭连接。JSON `tool_calls` 的可选 `index` 必须为非负安全整数；SSE 用它聚合分片，不传入工具参数。
- 对 `deepseek-*` 模型显式发送 `thinking: {"type":"disabled"}`，其他模型不添加该参数；上游若仍返回非空 `reasoning_content`，继续拒绝响应。
- 本机网关错误只显示允许列表中的固定原因码，不输出请求或上游错误正文。

**私密工具**

- 工具只接受完整隐私引用，`private_compute` 要求保留尖括号；缺少分隔符会拒绝。返回对象的所有字段均复验，额外原值字段也会拒绝。
- `privateToolsOnly` 同时限制执行权限和发给模型的工具目录，包括 Harness 后续注入的 agent 局部工具。
- `date_diff` 需要两个日期引用，计算第二个日期减第一个日期的天数；只接受 `unit=days`，不支持 `reference: today` 或隐含当前日期。
- `sum`、`mean`、`min`、`max` 不接受 options；`aggregate` 可指定 `operations`。无效参数返回固定工具错误，由模型更正或向用户补问，适配器不填造引用。

**命令行显示**

`censorprivacy dsh run` 通过私有子进程管道即时转发适配器取得的显示文字；原生 headless 总结保留在本机诊断文件，终端不重复打印。Web 使用 DSH 原生事件。`cliStream` 由 CensorPrivacy 管理，外部自定义 profile 不应手动启用。

</details>

## 相关文档

- [CensorPrivacy 首页](../../README.md)：完整包、初始化与网关启动。
- [使用手册](../../docs/usage.md)：DSH 准备、CLI/Web 操作及演示。
- [脚本速查](../../docs/scripts.md)：构建、回归与浏览器验收。
- [DSH 官方快速入门](https://deepseek-harness.github.io/deepseek-harness/en/guide/quickstart#configure-a-model)：官方模型配置与使用方式。
