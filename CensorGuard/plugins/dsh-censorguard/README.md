# dsh-censorguard

`dsh-censorguard` 是 Censorguard 的 DeepSeek Harness Host + Browser 插件。它替换 Harness 默认的本地 subprocess provider，增加工具调用前的 Intent Guard，并提供 Session 盾牌、策略热更新、事件查看、诊断以及可选的非 root mTLS gateway。

## 当前覆盖范围

- `tools/pre-execute`：在文件和网络工具真正执行前调用 daemon `evaluate_intent`。
- `tools/execute`：使用 `AsyncLocalStorage` 保存并行 Tool call 的 Session、scope 和 profile 上下文。
- `ctx.subprocess`：继承 Harness 的 `LocalSubprocessRuntime`，在普通进程和 PTY 终端 argv 前插入 `censorguard-exec`。
- Bash、PowerShell、Terminal、Jobs 和从 Tool call 启动的 subagent 进程统一经过无竞态 launcher。
- 支持按 Session、workspace、agent preset 和父 Session 选择 profile。
- 暴露 `ctx.censorguard`，并注册同源 fenced HTTP/WS 管理 API。
- Browser 在设置侧边栏提供一个“安全概览”页面，集中展示 daemon 状态、当前拦截策略、策略树与 YAML 热更新；
  会话顶部与“对话”“轨迹”并列提供“审计”页签，实时展示拦截事件。
- 事件默认 DENY-only，支持有界 replay、去重、gap、drop counter 和指数退避重连。
- 策略支持 validate、expected revision apply、冲突提示和 rollback。
- `Censorguard-gateway` 以非 root 用户运行，通过 mTLS、证书 RBAC、限流和写审计代理 daemon。

## 运行链路

```text
Harness Tool call
  ├─ tools/pre-execute
  │    └─ evaluate_intent -> censorguardd control endpoint
  └─ tools/execute (AsyncLocalStorage)
       └─ ctx.subprocess.spawn / spawnTerminal
            └─ censorguard-exec
                 ├─ register_self -> censorguardd launch.sock
                 └─ execve 原目标程序（PID 不变）
                      └─ eBPF 按 scope -> policy group 拦截
```

Intent Guard 负责 Harness Node 进程内部直接执行的文件/网络操作；launcher + eBPF 负责 shell、终端、后台任务和 subagent 等独立进程。两层不能互相替代。

## 前置条件

1. Linux Host 已安装并启动 `censorguardd launch`。
2. `/usr/bin/censorguard-exec` 存在且可执行。
3. daemon 同时监听：

   - `/run/censorguard/ctl.sock`
   - `/run/censorguard/launch.sock`
   - `/run/censorguard/events.sock`

4. 使用 `censorguardctl --policy base.yaml --domain <name>` 或 `--pid <pid>` 应用全局规则。
5. Harness Host 用户有权连接这些 socket。

可先用本仓库示例策略：

```bash
sudo /usr/sbin/censorguardd launch \
  --ctl-sock /run/censorguard/ctl.sock \
  --launch-sock /run/censorguard/launch.sock
```

## 安装与构建

开发环境：

```bash
cd plugins/dsh-censorguard
npm install
npm run typecheck
npm test
npm run build
npm pack
```

`@deepseek-ai/dsh-subprocess-local` 依赖原生模块。正式 Harness 安装通常会由整套应用完成这些依赖；单独构建本插件时，如果 Koffi 没有找到平台预编译包，需要安装 CMake，或者确保安装了当前平台的 `@koromix/koffi-linux-x64`。仅做源码类型检查时可以使用 `npm install --ignore-scripts`，但这种安装方式不代表运行时原生模块已经就绪。

构建产物位于 `lib/`。插件描述文件是 `dsh.plugin.json`，bundle patch 是 `cordis.patch.yml`。发布 tarball 内置离线运行所需的 `ws` 和 `zod`；Harness peer dependencies 由 Harness 自身提供。

## 安装到 DeepSeek Harness

在本仓库根目录执行下面这一行即可完成“离线构建插件 + 安装到 Harness web profile”（把路径替换为实际 Harness 根目录）：

```bash
pnpm dsh plugin --profile web add \
  /root/Censorguard/dist/dsh-censorguard-0.4.0.tgz
```

如果要安装到其他 profile，可追加第二个参数：

```bash
bash scripts/install-dsh-censorguard.sh /root/deepseek-harness headless
```

脚本不会安装或修改系统级 daemon、BPF、systemd 或 `/etc/censorguard` 文件；这些组件仍需按部署手册单独安装。

以下命令应在 DeepSeek Harness checkout 根目录执行，而不是在插件目录执行。若
`dsh` 没有安装为全局命令，使用 `pnpm dsh` 也完全可以。

先确认当前 shell 使用的是 Node 22 和 pnpm 11（`pnpm dsh` 会继承当前 shell 的
`PATH`）：

```bash
export PATH=/opt/node22/bin:$PATH
hash -r
node --version       # 应为 v22.x
pnpm --version       # 应为 11.x
cd /root/deepseek-harness
```

### 首次安装（本机离线 tarball）

```bash
pnpm dsh plugin --profile web add \
  /root/Censorguard/dist/dsh-censorguard-0.4.0.tgz
```

`add` 实际上是在 web profile 目录执行一次 `pnpm add`，然后把声明了
`dsh.bundle` 的包加入该 profile 的 bundle 列表。默认 profile 目录是：

```text
${DSH_HOME:-$HOME/.dsh}/profiles/web
```

### pnpm 的原生依赖审批

pnpm 10/11 默认不会自动执行依赖的 `postinstall`/`install` 脚本。安装时如果看到：

```text
ERR_PNPM_IGNORED_BUILDS
Ignored build scripts: ... dsh-subprocess-local ... koffi ... node-pty ...
```

这表示 pnpm 的安全策略阻止了原生模块构建，不是 Censorguard 策略或 daemon
错误。进入 profile 目录运行交互式审批：

```bash
DSH_HOME="${DSH_HOME:-$HOME/.dsh}"
PROFILE_DIR="$DSH_HOME/profiles/web"
cd "$PROFILE_DIR"
pnpm approve-builds
```

在列表中勾选本次安装输出里列出的构建依赖（通常包括
`@deepseek-ai/dsh-subprocess-local`、`esbuild`、`koffi`、`node-pty`），按回车确认，
然后重新安装 profile 依赖：

```bash
pnpm install
```

这些脚本分别用于终端/子进程运行时、JavaScript 打包和本地 FFI/PTY 支持。只做
源码检查时可以继续使用 `--ignore-scripts`，但这种安装不能用于启动真实 Web
Harness。

如果同时看到：

```text
Moving ... installed by a different package manager to node_modules/.ignored
```

说明该 profile 的旧 `node_modules` 曾由 npm 或另一版本的 pnpm 创建。pnpm 会先把
冲突目录移到 `.ignored`，再建立自己的依赖链接；这通常不是安装失败原因，也不会
影响 `package.json` 中的插件声明。完成 `approve-builds` 后重新执行 `pnpm install`，
即可让 profile 收敛到当前 pnpm 的依赖布局。

### 验证插件是否已加入 profile

```bash
DSH_HOME="${DSH_HOME:-$HOME/.dsh}"
PROFILE_DIR="$DSH_HOME/profiles/web"
grep -n -i -C 2 'dsh-censorguard' \
  "$PROFILE_DIR/package.json"
cd /root/deepseek-harness
pnpm dsh web --dump-config | grep -i -C 3 Censorguard
```

`package.json` 中应同时出现依赖项和 `dsh.profile.bundles` 项。`--dump-config` 不应
再报告 `unknown group`、`patch skipped` 或重复 `ctx.subprocess`；随后启动：

```bash
pnpm dsh web
```

启动前请确认 `censorguardd`、`ctl.sock`、`launch.sock` 和 `events.sock` 已经正常
运行；系统组件的安装方式见仓库 [部署手册](../../docs/current-runbook.md)。

### 当前失败安装的恢复方式

如果 `add` 已经把包写入 `package.json`，但最后因 `ERR_PNPM_IGNORED_BUILDS` 退出，
不必先删除包。直接审批并重新安装即可：

```bash
DSH_HOME="${DSH_HOME:-$HOME/.dsh}"
PROFILE_DIR="$DSH_HOME/profiles/web"
cd "$PROFILE_DIR"
pnpm approve-builds
pnpm install
```

如果审批后仍要重新执行一次添加，重复执行 `add` 是幂等的：

```bash
cd /root/deepseek-harness
pnpm dsh plugin --profile web add \
  /root/Censorguard/dist/dsh-censorguard-0.4.0.tgz
```

### 卸载插件

先停止正在使用该 profile 的 Web Harness，再从 profile 中移除包：

```bash
export PATH=/opt/node22/bin:$PATH
hash -r
cd /root/deepseek-harness
pnpm dsh plugin --profile web remove dsh-censorguard
```

`remove` 会调用 profile 内的 pnpm，删除依赖并同步移除 bundle 列表中的
`dsh-censorguard`。确认已移除：

```bash
DSH_HOME="${DSH_HOME:-$HOME/.dsh}"
PROFILE_DIR="$DSH_HOME/profiles/web"
grep -n -i 'dsh-censorguard' "$PROFILE_DIR/package.json" \
  || echo 'dsh-censorguard 已从 web profile 移除'
```

如果 profile 中还残留旧的 `node_modules`，可以在确认没有其他进程使用该 profile 后
重新安装依赖（不要手工删除整个 `~/.dsh`）：

```bash
cd "$PROFILE_DIR"
pnpm install
```

卸载插件不会卸载系统级 `censorguardd`、`censorguard-exec`、BPF object 或
`/etc/censorguard/base.yaml` 等策略文件。只有确认所有 Harness Session 都已停止使用
Censorguard 后，才按部署手册的系统卸载章节停止 daemon 并执行 `sudo make uninstall`。

### 升级或回滚插件

升级使用新的 tarball 重新执行 `add`，pnpm 会更新 profile 中的版本：

```bash
cd /root/deepseek-harness
pnpm dsh plugin --profile web add /absolute/path/dsh-censorguard-NEW.tgz
```

升级后重启 Web Harness。回滚时对旧 tarball 执行同一命令即可。若新版本增加了
新的原生依赖，pnpm 会再次提示 `approve-builds`。

## Harness 配置

`cordis.patch.yml` 先禁用 base bundle 的 `@deepseek-ai/dsh-subprocess-local`，再以独立的 loader id 插入 Censorguard Provider。include patcher 不允许通过 id-targeted patch 直接改写模块 `name`；Censorguard Provider 本身仍以 `subprocess` 服务名提供能力，因此运行时只有一个 `ctx.subprocess` owner。

```yaml
- id: subprocess
  name: '@deepseek-ai/dsh-subprocess-local'
  disabled: true

- insert:
    - id: Censorguard-subprocess
      name: dsh-censorguard
      config:
        mode: local
        transport: grpc
        assignmentStorage: storage-domain
        controlSocket: /run/censorguard/ctl.sock
        launchSocket: /run/censorguard/launch.sock
        eventSocket: /run/censorguard/events.sock
        launcherPath: /usr/bin/censorguard-exec
        requestTimeoutMs: 10000
        requestBodyLimitBytes: 1048576
        eventReplayCapacity: 10000
        eventSubscriberCapacity: 1000
        defaultProfile: default
        systemProfile: default
        profiles:
          default:
            label: Default
            group: __base__
            failurePolicy: deny
            unknownToolPolicy: deny
        sessionAssignments:
          session-id-1: strict
        presetAssignments:
          coding-agent: strict
        workspaceAssignments:
          /srv/important-project: strict
```

系统 daemon/eBPF 和可选 gateway 的部署步骤见 [Censorguard × DeepSeek Harness 部署手册](../../docs/current-runbook.md)；本插件自身的安装、审批、升级和卸载以本 README 为准。

### Profile 字段

- `group`：固定使用全局规则组 `__base__`；规则由 `censorguardctl --policy` 统一发布。
- `failurePolicy`：插件无法连接 daemon 或响应不完整时的处理。
  - `deny`：fail-closed，拒绝 Tool call 或禁止 launcher 执行目标程序。
  - `allow-with-audit`：允许继续并写 Host warning；应只用于明确接受降级风险的观测环境。
- `unknownToolPolicy`：无法转换为结构化 intent 的第三方工具如何处理。

策略规则本身返回的 `DENY` 不受 `failurePolicy` 影响；`allow-with-audit` 只处理授权基础设施故障，不能把真实拒绝变成允许。

### Profile 选择顺序

对有 Agent 所有者的 Tool call，优先级如下：

1. Session assignment
2. 父 Session assignment（子 Agent 默认继承）
3. 最长匹配的 workspace assignment
4. Agent preset assignment
5. `defaultProfile`

没有 Agent 所有者的 Host 内部进程使用 `systemProfile`；未配置时回退 `defaultProfile`。Session scope 格式是 `dsh-session:<session-id>`，Host 内部 scope 是 `dsh-system:<host-pid>`。

`assignmentStorage` 默认且推荐为 `storage-domain`：Browser 动态修改以及 Session 创建/恢复时计算出的初始 assignment 会写入 Harness 的 `Censorguard` domain，Host 重启后恢复。Headless 部署如果没有 storage-domain，必须显式设置 `assignmentStorage: memory`；该模式只适合接受重启丢失动态 assignment 的临时运行，插件不会在缺少 storage-domain 时静默回退。

`mode: remote` 仍使用本机 `LocalSubprocessRuntime` 和本机 `launchSocket`。因此配置必须显式包含 `remote.coLocatedEnforcement: true`，且只能在 gateway 所代理的 daemon 与本机 launcher/目标进程属于同一 enforcement 节点时设置。Censorguard 不支持用远程 gateway 拦截另一台机器上的本地子进程。

## Intent 映射

| Harness 工具 | daemon intent | 说明 |
| --- | --- | --- |
| `read`, `read_file` | `file.read` | 相对路径按 Session cwd 转为绝对路径 |
| `write`, `write_file` | `file.write` | 同上 |
| `str_replace_editor` | `file.read` / `file.write` | `view` 为 read，其余编辑命令为 write |
| `apply_patch` | write / rename / delete | 从 patch 文件头提取全部目标 |
| `web_fetch` | network | 提取 URL host、scheme 和端口 |
| `bash`, `pwsh`, `terminal` | launcher + eBPF | 不在应用层解析 shell 字符串 |
| 未识别第三方工具 | unknown_tool | 由 `unknownToolPolicy` 决定 |

`web_search` 的真实目标由 provider 决定，查询文本本身不能可靠推导网络目的地，因此当前按 unknown tool 处理；不应伪造一个看似精确、实际错误的网络 intent。

## 开发验证

```bash
npm run typecheck
sudo npm test
npm run build
node -e "import('./lib/index.js').then(m => console.log(m.default.name))"
```

测试中的 Unix socket 用例在受限沙箱里可能收到 `EPERM`，应在允许创建本地 Unix socket 的环境运行。当前测试覆盖 intent 转换、profile 继承、并行上下文、RPC request-id、deny/fail-open、Browser reducer、trusted-host/Origin/body limit、revision conflict、event replay/filter/gap、gateway config/RBAC，以及真实 launcher 和 mTLS gateway 集成链路。

## 故障排查

- 启动即提示 launcher 不可执行：检查 `launcherPath` 及执行权限。
- Tool call 全部 fail-closed：检查 `controlSocket`、socket 用户组和 daemon health。
- shell 目标程序未启动：查看 `censorguard-exec` stderr，并检查 `launchSocket`。
- `unknown group`：插件 profile 的 `group` 不存在于 daemon 当前策略。
- 应用层允许但进程内仍被拒绝：这是 eBPF 的最终强制结果，检查 scope 当前绑定和内核审计事件。
- 修改 assignment 后旧进程未立即切换：在 Session 盾牌中勾选 `Rebind`；若调用 API，PUT assignment 时传 `rebind: true`。
- Browser 页面无事件：确认 `eventSocket`、Host 日志和 Diagnostics 中的 stream 状态；默认只显示 DENY，ALLOW 需要主动切换过滤器。
- gateway 写操作返回 `permission_denied`：daemon 必须以 `--gateway-user <service-user>` 显式信任 gateway UID；仅把用户加入 socket group 只允许连接，不授予策略写权限。


sudo lsof -nP -iTCP:3080 -sTCP:LISTEN
sudo kill -TERM 12345
pnpm dsh web --port 3081 --no-open
ssh -N -L 3081:127.0.0.1:3081 root@服务器IP
