# CensorPivot 从零安装与使用

本文档用于在一台 Linux 主机上从源码安装并运行完整 AgentCensor。CensorPivot 不是
CensorFS、CensorGuard、CensorScope 的替代品；它需要三个组件的二进制、配置和 daemon。

`cargo build` 和 `cargo test` 只生成、测试当前项目的文件，不会创建系统用户，也不会把
组件安装到 `/usr`。如果直接执行 README 中旧的手工目录命令，出现：

```text
install: 无效的用户 'censorpivot'
sudo: 未知用户 censorpivot
```

直接原因是 `censorpivot` 系统用户尚未创建，并不表示测试失败。下面的统一安装器会创建它。

## 1. 安装前提

支持的完整运行环境是 Linux、systemd、cgroup v2、FUSE、BTF 和已启用 BPF LSM 的内核。
需要以下构建工具：

- Rust/Cargo；CensorScope 当前要求 Rust 1.90+；
- `make`、clang 17+、`llvm-strip`、`bpftool` 和 libbpf 头文件；
- `sudo`、`systemd`、`useradd`、`groupadd`；
- CensorFS 持久化目录应位于本地 ext4 或 XFS，而不是 NFS、OverlayFS 或 tmpfs。

在仓库根目录执行诊断。首次安装前它会报告组件缺失，这是预期结果，并会显示修复命令：

```bash
cd /root/AgentCensor
sudo CensorPivot/scripts/install-agentcensor.sh doctor
```

诊断必须显示 `cgroup v2` 正常，才能执行 `start`。以下命令的第一行应输出
`cgroup2fs`，第二行应列出控制器：

```bash
stat -fc %T /sys/fs/cgroup
cat /sys/fs/cgroup/cgroup.controllers
```

如果当前系统仍使用 cgroup v1，在使用 GRUB 和 `grubby` 的 openEuler/RHEL 系统上可执行：

```bash
sudo grubby --update-kernel=ALL \
  --args="systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all"
sudo reboot
```

重启后重新执行上面的验证命令和 `doctor`。不要在正在运行的 cgroup v1 系统上直接覆盖挂载
`/sys/fs/cgroup`；这会破坏 systemd 已管理的服务。若运行在容器内，宿主机还必须向容器提供
可写的 cgroup v2 层级，否则应改在虚拟机或宿主机运行 AgentCensor。

## 2. 依次安装三个组件

安装器默认从相邻的 `CensorFs/`、`CensorGuard/`、`CensorScope/` 源码目录执行 release
构建，然后安装固定路径的二进制。每一步失败都会保留原始构建输出，并给出下一步提示。

```bash
cd /root/AgentCensor
sudo CensorPivot/scripts/install-agentcensor.sh fs
sudo CensorPivot/scripts/install-agentcensor.sh guard
sudo CensorPivot/scripts/install-agentcensor.sh scope
```

三个命令分别完成：

| 命令 | 主要安装结果 |
|---|---|
| `fs` | `/usr/local/bin/censorfs`、`/usr/libexec/censorfs/*`、存储和运行目录 |
| `guard` | `/usr/sbin/censorguardd`、`/usr/bin/censorguard*`、BPF 对象和 AgentCensor 策略 |
| `scope` | `/usr/sbin/censorscoped`、`/usr/bin/censorscopectl` 和 operator 配置 |

CensorGuard 安装时若提示 BPF LSM 未启用，安装文件仍可完成，但 daemon 无法正常运行。
需要将 `bpf` 加入内核 `lsm=` 启动参数并重启，直到下面命令的输出包含 `bpf`：

```bash
cat /sys/kernel/security/lsm
```

## 3. 安装 Pivot 和统一服务

三个组件成功后执行：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh pivot
```

该命令会：

- 创建系统组和非 root 系统用户 `censorpivot`；
- 安装 `/usr/local/bin/censord` 和 `/usr/local/bin/censorpivot`；
- 安装 `/etc/agentcensor/censord.json` 和 `/etc/censorpivot/config.json`；
- 安装只允许调用 CensorFS mounter 的 sudoers 规则；
- 安装 `agentcensord.service` 和 `censorpivot.service`。

也可以用一个命令完成第 2、3 节：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh all
```

安装器默认保留已有配置。确认要换成仓库当前示例时使用：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh --force-config pivot
```

覆盖前的配置会保存为同目录下的 `.bak` 文件。

## 4. 首次初始化与启动

首次启动会把 `/var/lib/agentcensor/workspace` 的当前内容导入为 CensorFS `main` 分支的
初始基线。如果需要使用已有工程，应在第一次 `start` 前将它放入该目录。一旦生成
CensorFS superblock，重复启动不会重新导入或覆盖基线。

```bash
sudo CensorPivot/scripts/install-agentcensor.sh start
```

`start` 会先检查 cgroup v2；前置条件不满足时不会启用或启动任何 AgentCensor service。
systemd 启动失败时，安装器会自动打印完整 service 状态和最近 100 行日志，不需要先猜测
失败组件。

`start` 执行以下顺序：

1. 停止已有的两个统一 service，并等待其管理的三个组件退出；
2. 拒绝与正在运行的 `censorfsd.service`、`censorguardd.service` 或独立 CensorScope 冲突；
3. 启动 `agentcensord.service`；
4. `censord init` 幂等初始化三个组件；
5. `censord run` 在前台统一监督三个 daemon，并等待三个控制面健康；
6. 幂等下发 `censorguard-dsh-default` 策略组；
7. 使用专用用户启动 `censorpivot.service`；
8. 执行两个 doctor 检查。

统一模式下不要再单独启用组件服务。它们会与 `censord` 争抢 Unix socket 和状态锁。

## 5. 状态检查和停止

日常检查：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh status
sudo CensorPivot/scripts/install-agentcensor.sh doctor
```

直接检查两个层次：

```bash
sudo /usr/local/bin/censord doctor --config /etc/agentcensor/censord.json
sudo -u censorpivot /usr/local/bin/censorpivot doctor
```

查看日志：

```bash
sudo journalctl -u agentcensord.service -n 100 --no-pager
sudo journalctl -u censorpivot.service -n 100 --no-pager
```

停止全部进程：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh stop
```

systemd 先停止 Pivot，再向统一 supervisor 发送 `SIGTERM`。`censord` 会终止三个独立
进程组、等待回收，超时后再发送 `SIGKILL`，避免组件残留。

## 6. 安装 DSH 组合插件

原生 daemon 正常后，再安装一个包含 FS、Guard、Scope 的 CensorPivot DSH 组合插件。
它需要 Node.js 22+、pnpm 和 `dsh`：

```bash
CensorPivot/scripts/install-dsh-censorpivot.sh
```

该步骤只配置 DSH `web` 和 `headless` profile，不安装原生 daemon，因此必须放在前面
的原生组件安装之后。若使用 `/root/deepseek-harness` 源码而没有全局 `dsh` 命令，可以直接
执行（`=` 两边不能有空格）：

```bash
DSH_ROOT=/root/deepseek-harness \
  CensorPivot/scripts/install-dsh-censorpivot.sh
```

也可设置 `DSH_BIN=/absolute/path/to/dsh` 指向真实可执行文件。新版安装器还会自动识别当前
DeepSeek Harness 源码目录，以及 AgentCensor 相邻的 `/root/deepseek-harness`。插件依赖使用
pnpm 的标准 store；若该目录不可写，可设置例如
`DSH_PNPM_STORE_DIR=/root/.dsh/pnpm-store` 后重试。

默认安装每次都会从当前三个组件源码重新生成组合包，避免旧 `dist/` 掩盖修复。只有显式设置
`CENSORPIVOT_PACKAGE_DIR` 或传入包目录参数时，安装器才复用预构建包。

错误写法 `DSH_BIN = /root/deepseek-harness/` 会被 Bash 当作执行名为 `DSH_BIN` 的命令。
此外，源码目录应配置为 `DSH_ROOT`；`DSH_BIN` 只用于真正的可执行文件或命令名。

安装完成后，从源码根启动 Web：

```bash
cd /root/deepseek-harness
pnpm dsh web
```

组合插件会识别当前源码根，并用同一个 `pnpm dsh` 入口启动每个 headless worker，不要求
另外安装全局 `dsh`。

## 7. 常见错误

### `无效的用户 'censorpivot'`

尚未执行 Pivot 系统安装。运行：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh pivot
```

### `CensorFS/CensorGuard/CensorScope is not installed`

按错误提示执行对应的 `fs`、`guard` 或 `scope` 子命令。`pivot` 会在构建前检查三个组件，
不会留下安装一半的 Pivot 配置。

### `BPF LSM is not enabled` 或 Guard 启动失败

确认 `/sys/kernel/btf/vmlinux` 存在，且 `/sys/kernel/security/lsm` 包含 `bpf`。修复内核
启动参数并重启后，再执行 `start`。仅安装 clang/bpftool 不能替代运行时 BPF LSM。

### `/dev/fuse` 缺失

```bash
sudo modprobe fuse
ls -l /dev/fuse
```

### `cgroup v2 is unavailable at /sys/fs/cgroup`

这不是组件缺失，而是当前内核启动方式仍未提供统一 cgroup v2。按第 1 节启用并重启；确认
`/sys/fs/cgroup/cgroup.controllers` 存在后，重新安装最新的 systemd 单元并启动：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh pivot
sudo CensorPivot/scripts/install-agentcensor.sh doctor
sudo CensorPivot/scripts/install-agentcensor.sh start
```

### `standalone service ... is already running`

统一 supervisor 与独立组件服务不能同时运行。例如：

```bash
sudo systemctl disable --now censorfsd.service censorguardd.service
sudo /usr/sbin/censorscoped stop
sudo CensorPivot/scripts/install-agentcensor.sh start
```

### `socket parent must be owned by uid 0 and not writable by group/other`

CensorGuard 会拒绝在组或其他用户可写的目录中创建安全控制 socket。使用最新版安装器重新
安装 unit；它会将 `/run/censorguard` 修正为 root 所有、模式 `0750`：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh pivot
sudo CensorPivot/scripts/install-agentcensor.sh start
```

### 配置仍指向旧的 `/usr/local/bin` 路径

已有配置会被保留。先对比 `.example.json`，确认没有自定义内容需要保留，再执行：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh --force-config pivot
```

### `cargo test` 显示两个 Guard launcher 测试为 ignored

这是正常结果。这两个测试明确要求已经安装真实 `censorguard-exec`，普通单元测试不会自动
运行它们；`test result: ok` 表示测试成功。完整主机安装后可按测试提示单独运行 ignored 测试。
