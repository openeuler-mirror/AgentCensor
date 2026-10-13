# CensorGuard 0.5.0 DSH 插件指南

本页负责把 `@censorguard/dsh` 安装到 DeepSeek Harness 的 profile，并在 Web 中使用。
系统构建、daemon 安装与升级统一见 [构建指南](构建指南.md)。当前适配验证对象为 **DSH 0.1.5-rc.2**。

## 快速安装

先按构建指南完成 `make setup`，确认 `sudo censorguardctl doctor` 全部通过。
以下前三步在 **CensorGuard 仓库根目录**执行；DSH 仓库本身应已安装依赖且可以运行 Web。

### 1. 发布 DSH 策略组

```bash
sudo censorguardctl policy apply --policy censorguard-dsh-default --file config/policy.dsh-default.yaml
```

策略组会持久保存。升级插件通常无需再次发布；重发会用文件内容更新该组，可能覆盖 Web 中的编辑。
示例 exec 黑名单只覆盖指定程序/参数写法；保护重要目录需补充 `file` 规则。
匹配范围和通配符限制见 [策略编写与保护范围](策略编写与保护范围.md)。

### 2. 给使用 DSH 的用户访问权限

普通用户需要加入 `censorguard` 组。将下面 `alice` 换为实际使用 DSH 的用户名，加入后重新登录：

```bash
sudo usermod -aG censorguard alice
```

以 root 运行 DSH 可跳过此步。插件安装应由实际使用 DSH 的用户执行，profile 存储在该用户目录下。

### 3. 一键构建并安装插件

```bash
make plugin-install DSH_REPO=/path/to/deepseek-harness
```

命令自动构建并打包 0.5.0 插件、移除旧四包依赖、安装单包到当前用户的 `web` profile。
默认使用 `~/deepseek-harness`，其他 profile 用 `DSH_PROFILE=名称`；自定义数据目录沿用 `DSH_HOME`。
首次插件依赖尚未缓存时，先运行 `make deps`。此步骤无需 sudo。

### 4. 检查配置并启动 Web

```bash
node plugins/dsh-censorguard/bin/dump-config.mjs --dsh /path/to/deepseek-harness
cd /path/to/deepseek-harness
pnpm dsh --profile web
```

配置检查应显示三个插件入口和八个依赖闸门全部通过。打开终端显示的 Web 地址，
首次打开点内测声明中的“继续”，选择工作区并创建或打开会话。

完成后应看到：

- 设置中的“安全策略”，保护状态为 `protected`。
- 会话中的“对话”“轨迹”“安全拦截审计”页签。
- DSH 原生会话侧边栏与文件预览。

审计显示实际产生的事件，初始为空是正常情况。DSH 主进程及子孙都纳入保护，
基线 `__base__` 与 `censorguard-dsh-default` 组共同生效。

## 更新与停止

| 更新内容 | 操作 | 是否需要重启 DSH |
| --- | --- | --- |
| 插件源码 / 0.5.0 插件包 | 再执行 `make plugin-install DSH_REPO=...` | 是，随后刷新浏览器 |
| daemon / ctl / grpc / BPF / unit | 在 CensorGuard 根目录执行 `make setup` | 自动重新接入；关注 Web 保护状态 |
| 基线或 daemon 配置 | 修改 `/etc/censorguard/base.yaml`，执行 `sudo censorguardd reload` | 自动重新接入 |
| DSH 附加组规则 | Web 保存，或 `policy apply` 发布该组 | 立即应用 |

插件更新需要先停止旧 DSH Web 进程，再启动新的进程；浏览器刷新会获取新资源。
停止 DSH 可在启动终端按 Ctrl+C；daemon 服务独立运行。

## 展开查看细分步骤

<details>
<summary>1. 系统前提与策略权限</summary>

```bash
sudo censorguardctl status
sudo censorguardctl doctor
id
```

默认配置提供：

- `daemon.dsh_self_groups: [censorguard-dsh-default]`，允许 DSH 自身绑定这个组。
- `daemon.spawn_grpc: true`，由 daemon 启动适配器，监听 `127.0.0.1:50051`。
- Unix socket 所在目录供 `censorguard` 组访问。

如果使用保留的旧配置，需核对上述设置。修改 daemon 配置需要 `sudo censorguardd reload`。
组文件只有 `rules`，通过 `policy apply` 发布；全局基线位于系统启动配置中。
Web 与 gRPC 只管理授权的附加组，不能更改基线或 daemon 开关；清空附加组仍保留基线。

</details>

<details>
<summary>2. 插件构建、安装包与 profile 位置</summary>

```bash
make plugin
# 等价的安装入口：
bash scripts/install-dsh-censorguard.sh /path/to/deepseek-harness web
```

构建生成 `dist/censorguard-dsh-0.5.0.tgz`。安装器调用 DSH CLI，把安装包加入
`${DSH_HOME:-~/.dsh}/profiles/web/`，不会把插件复制到上游源码树。

已有安装包时，也可在 DSH 根目录手动执行：

```bash
pnpm dsh plugin --profile web add --ignore-scripts /path/to/CensorGuard/dist/censorguard-dsh-0.5.0.tgz
```

旧版本四包迁移由一键安装器处理。手动迁移时，先移除已安装的
`@censorguard/dsh-bundle`、`@censorguard/dsh-bootstrap`、`@censorguard/dsh-host`、
`@censorguard/dsh-client`，再添加单包 `@censorguard/dsh`。

发布包已包含构建产物，运行依赖为纯 JavaScript。安装器使用 `--ignore-scripts`，
避免新 profile 的 pnpm 配置因 protobufjs 的可选 postinstall 中断安装。

开发者也可以把源码目录加入 profile：

```bash
pnpm dsh plugin --profile web add /path/to/CensorGuard/plugins/dsh-censorguard
```

这会使用指向源码目录的链接。此方式更新时执行 `make plugin`，再重启 DSH 并刷新浏览器。
安装包方式更新时必须重新安装包；两种方式的依赖来源可在 profile 的 `package.json` 中核对。

</details>

<details>
<summary>3. 配置组装检查与自定义 profile</summary>

```bash
node plugins/dsh-censorguard/bin/dump-config.mjs --dsh /path/to/deepseek-harness --profile web
```

检查工具按 `--dsh`、`DSH_BIN`、PATH 上的 `dsh` 查找 CLI，不会自动下载其他版本。
需要检查其他 profile 时修改 `--profile`，启动 Web 的 `--profile` 必须一致。

安装后，包中的 `dsh.bundle` 自动叠加 `cordis.patch.yml`。配置包含：

| 入口 | 作用 |
| --- | --- |
| `censorguard-bootstrap` | 启动时自行接入，提供保护状态与就绪依赖 |
| `censorguard-host` | 组策略与事件的 gRPC 桥、Web RPC 路由 |
| `censorguard-ui` | 安全设置和会话审计页签 |

0.5.0 补丁使用当前 DSH 的 `typert-gateway`。浏览器依赖当前 connection、renderer、
settings 和 conversation 插件。检查缺少入口时会明确报错，避免旧补丁静默失效。

</details>

<details>
<summary>4. 保护如何生效，以及 daemon 重启后的行为</summary>

```text
DSH 启动 → Bootstrap 连接 dsh.sock → 当前进程自行接入 → 子孙继承保护
Web 策略 → Host → gRPC → ui.sock → 授权组更新
BPF 事件 → events.sock → gRPC → Host 缓存 → Web 审计
```

daemon 通过 Unix socket 的进程凭据确认 DSH 身份。gRPC 用于策略管理和事件读取，
进程接入由 Bootstrap 直接完成。插件保留官方 subprocess 和其他原生功能，并添加就绪依赖。

默认 `blockOnFailure: false`：daemon 不可达、组尚未发布等情况会显示 `degraded`，
DSH 仍可启动，后台重试；恢复后转为 `protected`。需要接入失败就阻止启动的部署，可将
bundle 的 Bootstrap 配置改为 `blockOnFailure: true`，修改后重新构建安装。

Bootstrap 每 5 秒核对 daemon boot ID；重启后重新接入。保护在 daemon 停机期间中断，
以恢复后的 Web 状态为准。DSH 根进程退出后，daemon 回收相应追踪域。

</details>

<details>
<summary>5. Web 安全设置、审计和原生会话功能</summary>

安全策略设置展示当前保护状态和绑定组规则。校验只编译，保存会立即发布组规则；
版本检查发现冲突时，重新读取后再编辑。写接口只允许本机 loopback 浏览器，远程访问只读。

“安全拦截审计”位于已打开会话的页签区，范围是**当前 DSH 实例及其子孙进程**，
包含各会话工具、启动加载和页面文件访问。页签放在会话中，不代表事件只属于这一段对话。
可按事件类型、允许/拒绝、进程和目标筛选，选中条目查看详情。
切走取消浏览器长轮询，Host 继续接收；回来恢复缓存。daemon 重启后切换到新事件编号。

| 字段 | 含义 |
| --- | --- |
| 事件序号（旧版 `seq`） | daemon 当前启动期间分配的全局事件编号，放行也有编号；其他域和显示过滤会造成跳号，不能当作命令数、拒绝数或丢失数 |
| 传输丢失 | daemon 显式报告的本订阅传输丢失；断线期间数量未知，内核丢失另用 `censorguardctl status --json` 的事件计数诊断 |
| 缓存移除（旧版“淘汰”） | `Host / 浏览器` 两个缓存各自移除的旧记录数；两者可能重叠，不相加，也不代表保护失效 |

Host 上限为 **5000 条 / 10 MiB**，浏览器上限为 **5000 条**。这是实时观察窗，
没有审计日志持久化；缓存满后移除旧记录，因此尽量在测试前打开页签。
登录后 DSH 会加载资源、访问工作区；开启全局放行审计时即使未开始对话也会产生事件。

为避免“接收审计 → 产生网络审计 → 再接收”的循环，插件订阅会过滤 **DSH Host 本进程
未命中规则的 ALLOW 网络 send/recv 包事件**。该进程的网络连接事件、命中规则事件、
所有 DENY、文件/执行事件以及子进程事件仍保留。内核检查与 daemon 原始事件不变；
需要观察完整网络包事件时使用独立终端的 `sudo censorguardctl audit --kind net`。
此修复需要**同时更新 censorguard-grpc 和插件**；只换浏览器资源不足以修复旧适配器的循环。

对话、轨迹、会话树和文件预览由 DSH 提供。初始首页没有打开会话时，不会出现会话页签。
实际文件访问受策略检查，因此被拒绝文件可能在预览时提示权限错误，并产生对应审计记录。

</details>

<details>
<summary>6. 回归检查与卸载</summary>

```bash
make plugin-test
make integration-dsh-web DSH_REPO=/path/to/deepseek-harness
```

Web 验收需要上游 Web 已构建、Playwright Chromium 和浏览器运行库，以及 root/BPF LSM。
测试使用临时 profile、state、端口及本地模型桩，覆盖对话、轨迹、侧边栏、
真实对话工具 `read` / `bash cat` / `bash rm` 的 BPF 拒绝及 Web 审计、
开启全部放行审计后的空闲事件循环检查，以及 reload 后自动重新接入。
测试只在临时会话中切换 DSH 权限模式，让命令到达 BPF 检查，不改变用户 profile。
结果见 [策略管理验收](策略管理验收.md)。

在 DSH 根目录卸载：

```bash
pnpm dsh plugin --profile web remove @censorguard/dsh
```

随后重启 DSH，插件的补丁和 Web 扩展会移除。系统 daemon 及已发布组独立保留。

</details>

## 查看已安装插件与清理旧版本

在 DSH 仓库目录执行，`--profile` 与启动时保持一致：

```bash
pnpm dsh plugin --profile web list --depth 0
```

命令列出插件包名和实际版本。profile 位于 `${DSH_HOME:-$HOME/.dsh}/profiles/web/`，
其 `package.json` 中 `dependencies` 表示安装来源，`dsh.profile.bundles` 表示启用的补丁层。
同一包名在一个 profile 中通常只有一个直接依赖版本；pnpm 缓存里的旧版本不等于被加载。
其他用户或其他 profile 的插件需要分别检查。

旧包名可能是 `dsh-censorguard`、`@censorguard/dsh-bundle`、`@censorguard/dsh-bootstrap`、
`@censorguard/dsh-host`、`@censorguard/dsh-client`。从列表中找到实际安装的旧包后移除，例如：

```bash
pnpm dsh plugin --profile web remove --ignore-scripts @censorguard/dsh-bundle
```

替换成实际旧包名，可一次传入多个包。保留新版 `@censorguard/dsh`，随后重启 DSH 并刷新浏览器。
一键安装器 `make plugin-install DSH_REPO=...` 已会清理上述旧包，通常不需要手动操作。

## 常见问题

| 现象 | 先检查什么 |
| --- | --- |
| 所有页签或侧边栏不见了 | 首次声明是否点“继续”；是否选择工作区并打开会话；是否启动了安装插件的 profile |
| 安全设置 / 审计页签缺失 | 运行配置检查；重新安装插件、重启 DSH、刷新浏览器 |
| 状态是 `degraded`，提示组不存在 | 用 root 发布 `censorguard-dsh-default`；插件后台会重试 |
| `EACCES` 访问 dsh.sock | 用户是否属于 `censorguard`，加入组后是否重新登录 |
| `ECONNREFUSED 127.0.0.1:50051` | `sudo censorguardctl doctor`；确认同版本 grpc 已安装及 spawn_grpc 已启用 |
| 缺失 `lib/client.js` | 执行 `make plugin-install`，完整重建并安装插件 |
| Web 保存失败或版本冲突 | 使用 loopback 地址；重新读取组；检查 daemon 与 grpc 是否同版本 |
| 审计为空 | 确认 protected，再产生受控进程的文件/执行/网络事件；核对 daemon 审计开关与规则 |
| 对话操作没有出现 DENY | 看轨迹中的真实工具结果：若 DSH 权限/沙箱在执行前拒绝，操作未到内核；若命令成功，核对实际域绑定组和目标规则；若 EPERM 且 ctl 有 DENY，则检查插件与 grpc 更新及页面过滤 |
| 登录后事件暴涨、拒绝记录消失 | 同时更新 grpc 和插件；旧版审计网络收发可能形成循环，将记录挤出缓存 |
| 重启后暂时 degraded / 审计出现 gap | 等待自动接入；持续失败时查看 daemon 和 DSH 日志 |

详细 CLI 语义见 [命令手册](命令手册.md)，系统部署问题见 [构建指南](构建指南.md)。

对话执行本地 `cat`/`rm` 时，放行程序启动可以是 `EXEC ALLOW`，
目标文件读取/删除被拒绝则是 `FILE DENY`。查文件拦截时选择 `FILE` 或“全部类型”，
不能只看 `EXEC`。原生 `read` 工具由 Node 读文件，线程名可能是 `node` 或 `libuv-worker`，
详情中的 `tgid` 对应 DSH 主进程；不是所有工具都通过独立命令执行。
默认真实 DSH 域绑定 `censorguard-dsh-default`；手工测试的 `dsh-web → web` 是另一个域，
仅把规则发布到 `web` 不会更改当前 DSH 保护规则。用 `sudo censorguardctl status` 核对实际绑定。
