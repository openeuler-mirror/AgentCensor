# DSH 插件指南

本文档描述 Censorguard DSH 插件的架构、安装、使用与故障排查。该插件为
DeepSeek Harness(DSH)提供**整树包裹模式**的安全保护:DSH 主进程启动时
自动 attach 到 daemon,此后整棵进程树(包括不经任何插件通道启动的子进程)
都在 eBPF LSM 强制之下。

系统构建见 [构建指南](构建指南.md);Censorguard 整体使用见 [使用手册](使用手册.md)。

## 1. 架构概览

### 1.1 整树包裹(fail-closed)

DSH 主进程启动时,最先激活的 Bootstrap 插件经 `dsh.sock` 调 `attach_self`
(pid 由 SO_PEERCRED 获取,不可伪造),daemon 将该进程注册为追踪根,此后
整棵进程树(fork 自动继承 + pending 降级层)都在强制之下。

attach 未成功时,`censorguardReady` 依赖闸门让 DSH 关键入口
(webserver / api-gateway / agent-loop / subprocess / tools 等)不进 Ready,
**DSH 启动失败**——这是有意的 fail-closed 语义:没有保护就不启动。

### 1.2 三条通路

```text
自助面: DSH Bootstrap → dsh.sock (0666) attach_self / status_self / tree_self
        5s 心跳比对 boot_id, daemon 重启即自动重 attach

委托面: DSH Host 插件 → censorguard-grpc (127.0.0.1:50051, 非特权)
        → ui.sock (0666) 白名单组策略读写 + 运行时开关切换
        (daemon 永不监听网络; systemd 下 daemon --spawn-grpc 自动派生守护)

事件面: eBPF ringbuf → daemon events.sock → gRPC 流
        → Host 有界缓存 (5000 条 / 10 MiB) → Client 长轮询 → WebUI 审计页签
```

事件带单调 `sequence`(从 1 起)、`daemon_boot_id`(区分 daemon 重启)与
`dropped_before`(累计丢失数)。断线、daemon 重启、ringbuf 满都会在审计
列表留下 gap 标记条目,不制造"日志完整"的错觉。

### 1.3 插件包结构

`plugins/dsh-censorguard/`(pnpm workspace):

| 包 | 职责 |
|---|---|
| `@censorguard/dsh-runtime` | attach 客户端、心跳守护、保护状态机(供 bootstrap/host 复用) |
| `@censorguard/dsh-bootstrap` | Cordis 入口,最先激活,attach 后提供 `censorguardReady` 服务 |
| `@censorguard/dsh-host` | gRPC 桥:注册 `/censorguard-read`(trusted-host)与 `/censorguard-admin`(loopback)两条 RPC 通道;审计事件流有界缓存 |
| `@censorguard/dsh-client` | 浏览器半:安全策略设置页 + "安全拦截审计"页签(esbuild bundle) |
| `@censorguard/dsh-bundle` | 安装器(censorguard-dsh-install/uninstall)+ cordis.patch.yml(插件行与依赖闸门) |

## 2. 前置条件

| 条件 | 验证方法 |
|---|---|
| daemon 已运行且带 `--spawn-grpc` | `systemctl status censorguardd`;`ss -tln | grep 50051` |
| gRPC 适配器已安装 | `ls /usr/bin/censorguard-grpc` |
| DSH 策略组已由 root 下发 | `sudo censorguardctl policy-dump \| grep censorguard` |
| 当前用户在 `censorguard` 组 | `id \| grep censorguard`(**加入后需重新登录才生效**) |
| DSH 仓库存在且 profile 已初始化 | `ls ~/deepseek-harness-master/apps/cli`, `~/.dsh/profiles/web/` |
| 插件已构建 | `plugins/dsh-censorguard/packages/*/dist/` 与 `packages/client/lib/` 存在 |

## 3. 安装步骤

### 3.1 构建插件

```bash
cd plugins/dsh-censorguard
pnpm install && pnpm build
```

### 3.2 root 一次性准备

```bash
# 下发 DSH 默认策略组(黑名单式,防 DSH 起不来;跨重启持久,只需一次)
sudo censorguardctl policy apply \
    --name censorguard-dsh-default \
    --file config/policy.dsh-default.yaml

# 把使用 DSH 的普通用户加入 censorguard 组(组内用户才能连各 socket)
sudo usermod -aG censorguard <用户名>
```

> 用户加入组后**必须重新登录**(或重启会话),否则 SO_PEERCRED 判定无
> socket 执行权,attach 报 `EACCES`。

### 3.3 安装到 DSH profile

普通用户执行:

```bash
node plugins/dsh-censorguard/bundle/bin/install.mjs            # 默认 profile=web
node plugins/dsh-censorguard/bundle/bin/install.mjs --dry-run  # 先看会执行什么
```

安装器行为:备份 profile `package.json` → `dsh plugin add`(pnpm add 四个
包,bundle 的 `dsh.bundle` 声明自动加入层列表)→ `--dump-config` 验证
(输出应含 censorguard-bootstrap/host/ui 三行)。幂等:已安装则跳过。

也可用 DSH 原生命令等价操作:

```bash
cd ~/deepseek-harness-master
pnpm dsh plugin --profile web add \
    plugins/dsh-censorguard/bundle \
    plugins/dsh-censorguard/packages/bootstrap \
    plugins/dsh-censorguard/packages/host \
    plugins/dsh-censorguard/packages/client
```

### 3.4 验证

```bash
pnpm dsh web        # 启动 DSH,日志应出现:
                    # [censorguard-bootstrap] attach 成功: domain=dsh-<uid>-<tgid>-<start> ...
                    # [censorguard-host] RPC 通道已注册: ...

# 或独立自检脚本(不启动 DSH)
node scripts/test/censorguard-dsh-selfcheck.mjs
```

attach 成功后域名形如 `dsh-<uid>-<tgid>-<starttime>`(多实例 DSH 可在
bundle 的 `cordis.patch.yml` 里给 bootstrap 加 `instanceHint` 区分)。

## 4. 使用方法

### 4.1 WebUI:安全策略设置页

DSH WebUI 的设置区出现"安全策略"区块:

- **状态条**:保护状态(protected/degraded)、域名、策略版本、daemon
  boot ID、hooks 健康度。
- **策略编辑器**:直接展示/编辑当前绑定组的 YAML 原文;**校验策略**只编译
  不生效;**保存并重新加载**带版本乐观锁(他人改过会提示冲突重新读取)。
  只能编辑当前绑定组,不能动 `__base__` 或其他组。
- **运行时开关**:六个 enable/audit 开关即点即切(等价 `ctl set`)。

权限边界:**只有本机(loopback)浏览器能调用写接口**,远程浏览器打开
WebUI 只读——写请求在 RPC 通道层就被拒绝(403)。

### 4.2 WebUI:安全拦截审计页签

会话视图的第三个页签"安全拦截审计"(对话、轨迹之后):

- 实时滚动被拦截/被审计的事件(FILE/EXEC/NET/GUARD),可按类型、
  允许/拒绝过滤,点击条目看完整详情(pid/comm/目标/参数/规则版本)。
- 顶部状态条:流状态(live/reconnecting)、已丢失事件数、缓存淘汰数、
  daemon boot ID。
- 缺口诚实展示:daemon 侧丢失(`dropped_before`)、断线期间丢失
  (sequence 跳变)、daemon 重启,都以 gap 标记条目出现在列表里。

### 4.3 域生命周期

- attach 成功即整树纳管;域由 daemon 按根进程退出自动回收,DSH 停止时
  无需任何清理。
- Bootstrap 每 5 秒心跳校验 daemon boot ID:daemon 重启后自动重新 attach;
  daemon 不可达时状态切 degraded 并指数退避重试,期间闸门入口不 Ready。
- DSH 插件更新(HMR/dispose)只断开连接,不解除纳管。

## 5. 卸载与更新

### 5.1 卸载

```bash
node plugins/dsh-censorguard/bundle/bin/uninstall.mjs                    # pnpm remove 四包
node plugins/dsh-censorguard/bundle/bin/uninstall.mjs --restore-backup   # 或从安装时备份恢复
```

patch 层消失后所有 inject 闸门回落到 DSH 原始配置,DSH 回到无保护启动。

### 5.2 更新插件代码

profile 里的 `@censorguard/*` 依赖是**指向插件工作区的符号链接**,更新
只需重建工作区产物后重启 DSH:

```bash
cd plugins/dsh-censorguard && pnpm build
# 重启 DSH(pnpm dsh web)
```

### 5.3 升级 daemon / gRPC 适配器

```bash
sudo systemctl stop censorguardd
sudo cp target/release/censorguardd /usr/sbin/ && sudo cp target/release/censorguard-grpc /usr/bin/
sudo systemctl start censorguardd
# DSH 心跳检测到 boot_id 变化后自动重 attach,无需重启 DSH
```

## 6. 常见问题

| 现象 | 原因与处理 |
|---|---|
| DSH 启动报 `references uninstalled group "censorguard-dsh-default"` | 策略组未下发(root 执行 §3.2 的 `policy apply`)。这是 fail-closed 闸门生效,不是 bug |
| DSH 启动报 `connect EACCES /run/censorguard/dsh.sock` | 用户不在 `censorguard` 组,或加组后未重新登录 |
| WebUI 连不上、报 `ECONNREFUSED 127.0.0.1:50051` | gRPC 适配器未运行:systemd unit 需带 `--spawn-grpc --grpc-bin /usr/bin/censorguard-grpc`(重装后 `daemon-reload` + restart);手动部署可 `sudo censorguard-grpc` |
| 安装/启动报 `Cannot find module '@censorguard/dsh-runtime'` | 插件 workspace 依赖未装,`cd plugins/dsh-censorguard && pnpm install` |
| WebUI 报 `Cannot find module .../lib/client.js` | client 浏览器 bundle 未构建,`pnpm --filter @censorguard/dsh-client build` |
| 保存策略报 `requires policy_yaml and expected_revision` | daemon 与 unit 版本不匹配的老部署问题(已修复),升级到 59d54d0 之后并同步二进制 |
| 策略保存显示"版本冲突" | 他人/他端已改过策略,点"重新读取"拿最新版本再改 |
| 审计页签大量 gap 条目 | 按 message 区分:daemon 报告丢失(ringbuf 满/消费慢)、断线重连、daemon 重启均正常记录;持续丢失说明事件量超出消费能力 |
| DSH 进程内存持续增长 | 旧版本审计流重连 bug(已在 59d54d0 修复),升级插件并重启 DSH |
| attach 成功但子进程没被拦 | 该进程在 attach 之前已存在且未 seed?重启用 `spawn` 启动或确认 `seed: true`(默认开) |
