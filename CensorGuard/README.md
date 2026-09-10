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

每个 bank 有 64 个 slot，slot 0 为全局基线，1..63 为域。10 维策略全部写完后才单次切换
active bank，因此失败 reload 不会产生“部分规则已新、部分规则仍旧”的窗口。

策略文件 schema 为根级 `rules`（全局基线）、可选 `groups`（命名规则组）与
`domains`（域到组的绑定）；每行直接声明允许/拒绝和可选审计：

```yaml
rules:
  - file deny+audit /etc/shadow
  - file deny+audit /root/.ssh
  - exec deny /usr/bin/nc
```

`enable_*`/`audit_*` 不再写入策略文件，而是通过 `censorguardctl set`（或 RPC v2
`set_switches`）调整的运行时开关：enable 默认全开、audit 默认全关，daemon 重启恢复
默认，reload 保持当前值。任一 audit 开关打开时该类 ALLOW 全量上报，单行规则的
`+audit` 始终生效，DENY 永远记录。

一个动态链接程序可能产生多个 `FILE ALLOW`（动态加载器、`libc`、locale 和配置文件的
读取），这表示文件打开检查点，不表示执行了多个可执行文件。只看执行事件可使用
`censorguard-audit launch --kind 2` 过滤。

## 构建

```bash
./scripts/check-env.sh
make bpf
cargo build --workspace --offline
cargo test --workspace --offline
```

启动 daemon 需要 root、BTF、BPF LSM 和 libbpf：

```bash
sudo target/debug/censorguardd launch \
  --bpf-object bpf/enforce.bpf.o
```

常用控制命令：

```bash
censorguardctl --policy config/base.yaml --domain lab-agent
censorguardctl --policy config/base.yaml --pid PID
censorguard-audit launch
```

0.4.0 最小启动验证：

```bash
cargo build --workspace --release --offline
make bpf
sudo ./scripts/test/startup-test.sh
```

## 验证

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
sudo make install-files
sudo systemd-sysusers /usr/lib/sysusers.d/censorguard.conf
sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/censorguard.conf
sudo systemctl daemon-reload
sudo systemctl enable --now censorguardd.service
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
- 委托面：Host 插件 → `censorguard-grpc`（127.0.0.1:50051，非特权，daemon 永不
  监听网络）→ `ui.sock`（0666）白名单组策略读写与运行时开关切换。systemd 部署下
  daemon 以 `--spawn-grpc` 自动派生并守护该适配器（崩溃 1s 后重启、daemon 退出时
  TERM→KILL 回收），无需单独的 grpc unit；手动部署也可单独运行
  `sudo censorguard-grpc`；
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
crates/censorguard-audit   审计客户端
crates/censorguard-grpc    非特权 gRPC 适配器（127.0.0.1:50051 → ui.sock/events.sock）
api/                         gRPC 契约 censorguard.v1
plugins/dsh-censorguard    DSH 整树包裹插件（runtime/bootstrap/host/client + bundle）
scripts/                     构建/打包/DSH 运维脚本
scripts/test/                可重复 root 集成测试与 DSH 验收脚本
packaging/                   systemd、sysusers、tmpfiles 配置
docs/                        架构、迁移、验证文档
```

## 文档

- [构建指南](docs/构建指南.md)：从源码构建全部组件与离线打包
- [使用手册](docs/使用手册.md)：核心概念、策略编写、运行时开关、观测与故障排查
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
