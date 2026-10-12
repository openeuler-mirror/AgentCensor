# Censorguard

Censorguard 是一个基于 eBPF LSM 的 Linux 进程级安全强制系统：

- Rust：严格策略编译、DNS、PID 域生命周期、事务热更、控制和审计；
- C：eBPF LSM/tracepoint 内核数据面；
- libbpf C shim：把所有 unsafe 和内核 fd 操作限制在单一 crate。

## 核心运行模型

```text
YAML -> Rust typed snapshot -> inactive policy bank
                              -> one active_bank switch
tgid -> domain/logical slot -> active bank inner maps
syscall -> LSM/tracepoint -> allow or -EPERM -> ringbuf audit
```

每个 bank 有 64 个 slot，slot 0 为全局基线，1..63 为公开组与多组组合的有效策略。规则先写入备用 bank 再切换；
域绑定与持久化另有阶段，不保证所有域同时切换。

启动配置只有 `daemon` 与 `rules`（全局基线）；附加组文件只有 `rules`，通过
`policy apply` 发布、`bind` 追加到 domain。每行声明允许/拒绝和可选审计：

```yaml
rules:
  - file deny+audit /etc/shadow
  - file deny+audit /root/.ssh
  - exec deny /usr/bin/nc
```

`enable_*` / `audit_*` 放在启动配置的 `daemon` 段，省略时 enable 全开、audit 全关；
仓库 `config/base.yaml` 显式开启三个 audit。
基线和 daemon 设置运行中不可热改，修改后用 `censorguardd reload` 完整重启。
附加组可热更新；domain 支持多个组，任一层 deny 即拒绝。`remove` 移除域内一个组，
`policy remove` 删除未被引用的组。`status` 显示简洁摘要，`status --json` 包含 PID 进程树，`doctor` 检查组件。

一个动态链接程序可能产生多个 `FILE ALLOW`（动态加载器、`libc`、locale 和配置文件的
读取），这表示文件打开检查点，不表示执行了多个可执行文件。只看执行事件可使用
`censorguardctl audit --kind 2` 过滤。

文件规则同时编译为 inode 身份和同一规则的规范化路径身份：内核先查 inode，inode
未命中时才使用路径兜底，以覆盖文件重建或新建后的短期窗口。当前 exec 参数规则依赖
execve/execveat 入口暂存 argv；仅当前可执行文件配置了参数规则且采集/暂存失败时，
`bprm_check_security` 按 fail-closed 拒绝该次执行（匹配 scratch 不可用仍拒绝）。
规则 token 最长 255 字节；运行时长参数不会匹配较短的完整 token，也不会绕过已命中的拒绝前缀。
参数规则最多 4 个 token（程序路径 + 前 3 个参数）；255 按 UTF-8 字节计算，
是项目限制，不是 Linux 完整路径或参数的系统上限。
脚本的参数缓存保留到解释器检查完成。网络策略覆盖
`socket_connect`、`socket_bind`、显式目标及已连接 socket 的 `socket_sendmsg`/`socket_recvmsg`。
TCP 入站由 `inet_conn_request` 在握手阶段检查：拒绝请求会丢弃，客户端通常超时。
监听 socket 由 listen/accept 和 attach 时的 fd 回填建立 scope 身份；reload 后须重新接入。
这仍不等价于包级防火墙。
文件 `allow` 不构成白名单；网络规则按带端口优先、同层级 deny 优先的顺序判定。
启动和 hook 的微秒级延迟尚未由现有启动 smoke test 证明。

## 构建

```bash
./scripts/check-env.sh
make deps
make all
```

`make all` 构建 Rust release、BPF 与 0.5.0 DSH 插件安装包。仅构建核心组件可加
`WITH_DSH=0`。一键系统安装使用 `make setup`，详见 [构建指南](docs/构建指南.md)。

常用控制命令：

```bash
censorguardctl --config config/base.yaml status
censorguardctl --config config/base.yaml policy apply --policy lab-policy --file group.yaml
censorguardctl --config config/base.yaml bind --domain lab-agent --policy lab-policy
censorguardctl --config config/base.yaml attach --pid PID --domain lab-agent
censorguardctl --config config/base.yaml audit
```

0.5.0 最小启动验证：

```bash
cargo build --workspace --release --offline
make bpf
sudo ./scripts/test/startup-test.sh
```

## 验证

快速手动验收见 [手动测试指南](docs/手动测试指南.md)，覆盖长参数、脚本、文件读取和策略热更新。

完整 root 集成：

```bash
make integration
```

当前已覆盖文件 read/write/delete/rename/chmod、link/symlink/mkdir/rmdir/mknod、
chown/xattr/ACL、预打开 fd 的 read/write/ftruncate/mmap/mprotect、命令与参数级 exec、多域隔离、
execveat 绝对/相对/AT_EMPTY_PATH 与可执行文件别名身份拦截、
IPv4/IPv6 静态网络与 A/AAAA DNS、reload/bind/SIGHUP/回滚、inner-map 共享、PID generation/fork/draining、
256-child PID churn、tracking map 满载 pending 降级、DNS 周期变化/stale-cache、
16 层目录继承/极深目录 fail-closed、完整守护钩子、非 root 授权、三层事件丢弃压力与
全局 audit 开关与按规则 `+audit` 的 ALLOW 审计、16 线程 48 万次文件压力、graceful shutdown。

## 安装与 systemd

```bash
make all
make setup
```

`make package` 可生成确定性 tar.gz 和 SHA-256。策略文件由 ctl 按需发布；socket 为
`0660 root:censorguard`，组成员仍受 SO_PEERCRED 授权限制。

## DSH 全局保护（整树包裹模式）

DSH（DeepSeek Harness）集成已从旧的“逐子进程保护”（替换 `ctx.subprocess` +
launcher argv 前插）升级为**整树包裹**：DSH 主进程启动时由最先激活的 Bootstrap
插件经 `dsh.sock` 调 `attach_self`（pid 取 SO_PEERCRED，不可伪造），daemon 将该
进程注册为追踪根，此后整棵进程树（fork 自动继承 + pending 降级层）都在 eBPF LSM
强制之下，不经任何插件通道的子进程也逃不掉。attach 未成功时插件切 degraded
态，DSH 照常启动并在 WebUI 提示保护未启用，后台重试待 daemon 就绪后自动恢复
（默认不阻塞；`cordis.patch.yml` 里 `blockOnFailure: true` 可恢复 fail-closed，
此时 `censorguardReady` 依赖闸门让 DSH 关键入口不进 Ready）。

三条通路：

- 自助面：Bootstrap → `dsh.sock`（0666）`attach_self`/`status_self`/`tree_self`，
  5s 心跳比对 boot_id，daemon 重启即重 attach；
- 委托面：Host 插件 → `censorguard-grpc`（127.0.0.1:50051，委托角色，daemon 永不
  监听网络）→ `ui.sock`（0666）白名单组策略读写。适配器由 root daemon 派生时继承
  root UID，接口权限由 adapter 与 ui.sock 角色限制，当前没有单独降权。systemd 部署下
  daemon 按配置文件中的 `daemon.spawn_grpc` 自动派生并守护该适配器（崩溃 1s 后重启、daemon 退出时
  TERM→KILL 回收），无需单独的 grpc unit；手动部署也可单独运行
  `sudo censorguard-grpc --config /etc/censorguard/base.yaml`；
- 事件面：`events.sock` → gRPC 流 → Host 有界缓存 → Client 长轮询，事件带
  单调 `sequence`（从 1 起）、`daemon_boot_id` 与 `dropped_before`。

插件位于 `plugins/dsh-censorguard/`（单包 `@censorguard/dsh`：runtime /
bootstrap / host / ui / client 五个内部模块 + cordis.patch.yml 补丁层），
安装方式：

```bash
dsh plugin --profile web add plugins/dsh-censorguard   # 装入 DSH profile
```

DSH 默认策略组 `config/policy.dsh-default.yaml`（黑名单式，防 DSH 起不来）由
root 下发，普通 DSH 用户经 attach 自助绑定。详见
[DSH 插件指南](docs/DSH插件指南.md)。

本机已执行并通过 Rust 格式、Clippy 严格门禁：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
```

## 目录

```text
bpf/                         C eBPF LSM 数据面
config/                      示例策略（含 DSH 默认组 policy.dsh-default.yaml）
crates/censorguard-common  C ABI 与 JSON-lines 协议
crates/censorguard-policy  YAML、文件、exec、IPv4/IPv6、A/AAAA DNS 编译
crates/censorguard-kernel  唯一 unsafe/libbpf 边界
crates/censorguard-daemon  特权 daemon、PID 域、热更新、角色 socket ACL
crates/censorguardctl      控制客户端
crates/censorguard-grpc    委托 gRPC 适配器（127.0.0.1:50051 → ui.sock/events.sock）
api/                         gRPC 契约 censorguard.v1
plugins/dsh-censorguard    DSH 整树包裹插件（runtime/bootstrap/host/client + bundle）
scripts/                     构建/打包/DSH 运维脚本
scripts/test/                可重复 root 集成测试与 DSH 验收脚本
packaging/                   systemd、sysusers、tmpfiles 配置
docs/                        架构、迁移、验证文档
```

## 文档

- [全流程摸底测试报告](docs/全流程摸底测试报告.md)：验收范围、实机证据、缺陷与修复
- [代码库结构与系统原理](docs/代码库结构与系统原理.md)：结构图、系统架构及模块实现分析
- [命令手册](docs/命令手册.md)：当前 CLI、命名与策略管理规范
- [构建指南](docs/构建指南.md)：从源码构建全部组件与离线打包
- [使用手册](docs/使用手册.md)：核心概念、策略编写、运行时开关、观测与故障排查
- [策略编写与保护范围](docs/策略编写与保护范围.md)：目录保护、exec 参数匹配与通配符边界
- [DSH 插件指南](docs/DSH插件指南.md)：DSH 整树包裹插件的架构、安装与使用

IPv6 规则示例：

```yaml
rules:
  - net deny 2001:db8::/32
  - net allow+audit [::1]:443
  - net deny api.example.com:8443
```

裸 IPv6/CIDR 表示无端口规则；带端口必须使用 `[IPv6/CIDR]:PORT`，避免冒号歧义。

更完整的手工实验策略见 [`config/standard-dev.yaml`](config/standard-dev.yaml)，
包含文件、命令和 IPv4/IPv6 网络统一规则；PID 绑定与审计步骤见
[使用手册](docs/使用手册.md)。
