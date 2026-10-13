# CensorPivot 从零安装与使用

本文档提供一个完整部署入口和对应的分步说明。CensorPivot 需要 CensorFS、CensorGuard、
CensorScope 的二进制、配置和 daemon；`cargo build` 本身不会完成系统部署。

## 0. 按脚本完成部署

在 **AgentCensor 仓库根目录**执行 [setup-agentcensor.sh](../scripts/setup-agentcensor.sh)。
脚本复用原有安装器，按照「准备主机 → 检查 → 安装四个项目 → 启动 → doctor」运行。

cgroup v2 是内核提供的统一控制层级，不是额外安装一个用户态组件。已经运行的 cgroup v1
主机需要调整内核引导参数并重启；脚本不会把一个临时 cgroup2 挂载覆盖到宿主上。

### 第一步：准备系统依赖和引导参数

以下命令适用于当前 openEuler/RHEL、使用 `grubby` 的主机：

```bash
cd /root/AgentCensor
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh prepare \
  --install-deps --configure-boot
```

该命令通过 `dnf`（或 Debian/Ubuntu 的 `apt-get`）安装原生编译依赖、加载 FUSE，检查
内核 BTF、cgroup、BPF LSM。如果需要切换引导方式，`--configure-boot` 会：

1. 读取 `grubby --info=DEFAULT`，保留原有 LSM 列表并补充 `bpf`；
2. 备份原始引导配置到 `/var/lib/agentcensor-boot-backup.XXXXXX/`；
3. 只调整默认内核的 `systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=...`；
4. 输出修改后的引导信息，以 **退出码 10** 提示需要重启。

看到 `REBOOT REQUIRED` 后，在维护窗口执行下面命令。不要用 `&& reboot` 串接，因为
退出码 10 表示“引导准备完成，当前内核尚未重新启动”，不是安装失败。

```bash
sudo reboot
```

已经启用 cgroup v2/BPF LSM 时无需重启，`prepare` 返回 0。不准备修改引导参数时，省略
`--configure-boot`；它只报告缺少的条件。`--install-deps` 也可省略，适用于依赖已准备好的
离线机器。缺少 `CONFIG_BPF_LSM/CONFIG_DEBUG_INFO_BTF` 等内核支持时应更换合适的内核，
仅修改引导参数不能添加内核能力。容器需要由宿主提供可写 cgroup v2，不能从容器修改宿主引导。

没有 `grubby` 的 Debian/Ubuntu 主机，先运行 `prepare --install-deps`，然后按下方
“手动切换 cgroup v2”步骤修改 GRUB 并重启。

### 第二步：准备 Rust 并检查重启后的环境

原生安装通过 sudo 执行，需要 root 环境能找到 Rust。已有 root rustup 的机器跳过下载；
否则从官方安装入口准备 rustup（只安装管理器，各项目所需 toolchain 由安装器下载）：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  -o /tmp/agentcensor-rustup-init.sh
sudo -H sh /tmp/agentcensor-rustup-init.sh -y --profile minimal --default-toolchain none
```

重启后回到仓库根目录：

```bash
cd /root/AgentCensor
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh prepare
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh check
```

重启后的 `prepare` 负责重新加载 FUSE 和检查 securityfs；`run` 也会确保这两项已准备。
检查项包括：运行中的 systemd、x86_64/aarch64、clang 17+ 及其 BPF 后端、libbpf/libelf/zlib
头文件与链接库、Rust 管理器、BTF、BPF LSM、FUSE，以及实际可写的 cgroup v2 和
`cgroup.kill`。临时检查 cgroup 会立即删除；CensorFS 持久化位置必须是本地 ext4/XFS。
检查失败时不会继续安装或启动服务。

### 第三步：安装、启动并运行真实批次

先退出正在运行的 DSH。若此前启用了独立组件服务，先停止并禁用它们；统一模式由
`agentcensord` 管理三个数据面：

```bash
# 只执行已存在的独立服务对应命令。
sudo systemctl disable --now censorguardd.service
# 如有独立 CensorFS service：sudo systemctl disable --now censorfsd.service
# 如有独立 Scope daemon：sudo /usr/sbin/censorscoped stop

sudo -H bash CensorPivot/scripts/setup-agentcensor.sh run --demo
```

`run` 按如下顺序执行，可重复运行：

| 阶段 | 实际动作 |
|---|---|
| 环境检查 | 重验本次启动的内核和工具链，不满足条件立即退出 |
| 服务交接 | 停止已存在的两个统一服务；拒绝仍在运行的独立组件 |
| 构建安装 | 调用 `install-agentcensor.sh all`，依次安装 FS、Guard、Scope、Pivot |
| 配置 | 保留已有文件、执行 Guard 0.5 迁移并校验组件连接一致性 |
| 启动 | 启用统一服务、初始化 FS、启动三个 daemon、下发组策略、启动非 root Pivot |
| 验证 | 执行 doctor；`--demo` 额外验证真实 View、工具执行和 Commit |

`--demo` 会往 CensorFS `main` 分支发布一个测试结果并推进 Head；只部署服务时省略它。
在首次运行前，将要导入的工程放到 `/var/lib/agentcensor/workspace`。已有 FS superblock
时不重新导入。已有自定义配置会保留，且可能需要按实际部署修改，不会自动恢复成示例。

### 第四步：同时安装 DSH 组合插件

为运行 Web 的用户准备 Node.js 22+、pnpm 和已完成依赖安装的 DSH `0.1.5-rc.2` 源码。
可以将第三步的命令替换为：

```bash
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh run \
  --dsh-root /root/deepseek-harness --demo
```

原生服务就绪后，脚本以发起 sudo 的用户重建并安装 Web/headless profiles；root 发起时
安装到 root 的 profile。DSH 检查和安装使用该用户的登录环境；使用 nvm 时，需先在该用户
的登录 shell 中确认 `node --version`、`pnpm --version` 正常，避免 sudo 的 `secure_path`
隐藏 Node。Web 放在终端前台启动，以便正常登录和查看日志：

```bash
export PATH="$HOME/.local/bin:$PATH"
cd /root/deepseek-harness
pnpm dsh --profile web
```

此脚本补齐部署环境和服务启动。**当前 DSH 普通 `read/bash` 仍未自动提交 Pivot
`BatchRequest`**，完整事务由 `--demo` 或原生批次接口验证。组件插件组合正常运行并不
代表普通对话已经具有统一 Prepare/Commit/Abort，详见 [验收报告](ACCEPTANCE.zh-CN.md)。

只需上述快速路径即可部署；下面是各阶段的独立命令和配置说明。

## 1. 安装前提

支持的完整运行环境是 Linux、systemd、cgroup v2、FUSE、BTF 和已启用 BPF LSM 的内核。
需要以下构建工具：

- Rust/Cargo，推荐安装 rustup。安装器使用各项目的 `rust-toolchain.toml`；未单独固定
  toolchain 的 Guard 和 Scope 使用 Pivot 固定的版本。缺少的 toolchain 会自动下载；
  即使 sudo 清理了 PATH，安装器也会识别当前用户的 `~/.cargo/bin/rustup`；
- `make`、clang 17+、`llvm-strip`、`bpftool` 和 libbpf 头文件；
- `sudo`、`systemd`、Python 3、`useradd`、`groupadd`；
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

### 手动切换 cgroup v2

如果当前系统仍使用 cgroup v1，在使用 GRUB 和 `grubby` 的 openEuler/RHEL 系统上可执行：

```bash
sudo grubby --update-kernel=DEFAULT \
  --args="systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all"
sudo reboot
```

若 BPF LSM 尚未启用，还需保留已有 `lsm=` 列表并添加 `bpf`；推荐使用上面的
`prepare --configure-boot` 自动完成两项配置及备份。

没有 `grubby` 的 Debian/Ubuntu：先备份 `/etc/default/grub`，在其
`GRUB_CMDLINE_LINUX` 保留原参数并追加
`systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all`。需要 BPF LSM 时，保留
`cat /sys/kernel/security/lsm` 或现有引导配置中的 LSM 列表，追加 `bpf` 后形成一个
`lsm=...` 参数，避免用新列表覆盖已有的 AppArmor/SELinux。然后执行：

```bash
sudo update-grub
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

覆盖前的配置会保存为同目录下的 `.bak` 文件。`--force-config pivot` 只覆盖 Pivot 和
supervisor JSON；要替换 Guard YAML，需要明确执行 `--force-config guard`。

### 从旧 Guard 接入升级

先退出 DSH，按以下顺序操作，避免新旧二进制同时使用状态目录：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh stop
sudo CensorPivot/scripts/install-agentcensor.sh guard
sudo CensorPivot/scripts/install-agentcensor.sh pivot
sudo CensorPivot/scripts/install-agentcensor.sh start
```

`pivot` 会自动调用 `censord migrate-guard`：将旧 `censord.json` 的 `policy`、socket、BPF、
state 和受支持的 `extra_args` 转入 Guard YAML；将 Pivot 的 `exec_command/launch_socket`
替换为共用的 `config/control`。规则和自定义组白名单保留，备份后缀为
`.pre-guard-0.5.0.bak`。迁移可重复执行；遇到未知选项或两个 launch socket 不一致时，
会在写入前拒绝，需先检查配置。单独执行迁移的命令是：

```bash
sudo /usr/local/bin/censord migrate-guard \
  --config /etc/agentcensor/censord.json \
  --pivot-config /etc/censorpivot/config.json
```

共用 YAML 必须可被 `censorpivot` 用户读取；安装器为默认 `/etc/censorguard` 使用
`root:censorpivot`、目录 `0750`、配置 `0640`。自定义路径的上级目录也应允许该用户遍历。
基础规则和 daemon 开关在启动时读取；动态组单独持久化，重启后仍会恢复。

### 组合配置在启动时如何加载

`/etc/agentcensor/censord.json` 是三个数据面的组合启动配置。它包含 CensorFS 的存储、
导入目录和 socket，引用 Guard YAML 和 Scope operator 配置，并指定各组件二进制。
它不会把三种配置语法合成一份策略文件。

| 配置 | 谁读取 | 控制内容 |
|---|---|---|
| `/etc/agentcensor/censord.json` | `censord init/run` | 三个组件的启动路径和监督参数 |
| `/etc/censorguard/agentcensor.yaml` | Guard；Pivot 读取连接配置 | `daemon`、基础 `rules`；动态组由 `policy apply` 发布 |
| `/etc/censorscope/censorscoped.conf` | Scope | 采集、存储、队列；组合 JSON 的 `level` 决定启动等级 |
| `/etc/censorpivot/config.json` | Pivot `serve` | 批次执行、组件连接、组白名单和事务存储 |

统一安装会为 Pivot 配置增加 `supervisor_config: /etc/agentcensor/censord.json`。`serve`
在监听请求前核对 FS socket、Guard 配置及 ctl、Scope socket、operator 配置及 ctl；
不一致时明确拒绝启动。`doctor` 同样检查该关系。手工独立部署可以省略此字段。
修改配置文件后需按下面的统一 stop/start 流程重新加载。

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

检查命令：

```bash
sudo CensorPivot/scripts/install-agentcensor.sh status
sudo CensorPivot/scripts/install-agentcensor.sh doctor
```

检查两个层次：

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

Guard 0.5.0 的 `reload` 会完整重启并清空进程登记。统一模式下修改基础 YAML 后，应先退出
DSH，再用安装器 `stop/start` 一起重启，随后重启 DSH；动态组修改使用 `policy apply`。
不要在批次执行中单独 reload Guard。

systemd 先停止 Pivot，再向统一 supervisor 发送 `SIGTERM`。`censord` 会终止三个独立
进程组、等待回收，超时后再发送 `SIGKILL`，避免组件残留。

## 6. 安装 DSH 组合插件

### 一键安装与日常启动

支持一键安装 FS、Guard、Scope 的 DSH 插件。环境已通过第 0 节的检查时，一并安装
原生组件、启动后台服务并安装两个 DSH profile：

```bash
cd /root/AgentCensor
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh run \
  --dsh-root /root/deepseek-harness
```

日常从 DSH 源码根目录启动 Web，使用安装 profile 的同一用户：

```bash
export PATH="$HOME/.local/bin:$PATH"
cd /root/deepseek-harness
pnpm dsh web
```

当前 DSH `0.1.5-rc.2` 将 `web` 作为 `--profile web` 的别名，两种写法加载相同的
profile。各命令的职责如下：

| 入口 | 实际作用 |
|---|---|
| `setup-agentcensor.sh run --dsh-root PATH` | 构建安装原生组件，启用并启动统一服务，安装 Web/headless 组合插件 |
| `install-dsh-censorpivot.sh` | 重建并安装三个组件的 DSH 插件和组合配置 |
| `pnpm dsh web` | 启动 Web，加载三个组件的 DSH 插件，连接已运行的后台服务 |

三个原生 daemon 由 `agentcensord.service` 统一监督，事务入口由 `censorpivot.service`
提供。Web 启动不会执行 `systemctl start` 或启动这些 daemon；关闭 Web 后，后台服务
继续运行。部署脚本已启用服务自启动，正常重启后可直接启动 Web。若此前手动停止了服务，
先执行 `sudo CensorPivot/scripts/install-agentcensor.sh start`。

`--demo` 是可选的原生事务验收，与插件安装和 Web 启动无关。当前普通对话的
`read/bash` 接入范围见第 0 节和[验收报告](ACCEPTANCE.zh-CN.md)。

### 仅安装或更新 DSH 插件

原生 daemon 正常后，再安装一个包含 FS、Guard、Scope 的 CensorPivot DSH 组合插件。
它需要 Node.js 22+、pnpm 和 DSH `0.1.5-rc.2`：

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

默认安装每次都会先重建 Guard 的 TypeScript 和浏览器 bundle，再从当前三个组件源码
生成组合包，避免旧 `dist/` 掩盖修复。只有显式设置
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

若更新了本仓库中的 Guard 或 Scope 插件，请重新执行上面的组合插件安装命令，并重启
`dsh --profile web`。只运行 `cargo build` 不会刷新 Web profile 中已安装的 npm 包。可以用
`pnpm dsh plugin --profile web list --depth 0` 核对：Guard 为 `0.5.0`，
Scope session proxy 至少为 `0.1.4`。源码版 DSH 的 Web 与 headless worker 应从同一
`DSH_ROOT` 启动；若启动 Web 的工作目录不是 DSH 源码根，请同时设置
`DSH_ROOT=/absolute/path/to/deepseek-harness`。

组合层使用 `typert-gateway`，并让工具服务、CensorFS 和 Scope session worker 等待
`censorguardReady`。默认 `blockOnFailure: true`：Guard 未就绪时阻止启动工具服务。
安装器会从 web/headless profile 清理旧的 Guard split packages；组件运行时包保留，
但独立 `dsh.bundle` 声明被移除，仅由组合层应用 patch。

查看已安装包和最终配置：

```bash
pnpm dsh plugin --profile web list --depth 0
pnpm dsh --profile web --dump-config
```

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

### runner 注册失败

运行 `censorguardctl --config /etc/censorguard/agentcensor.yaml status`，确认基础策略路径、
hooks 和 `censorguard-dsh-default` 策略组。检查 Pivot 与 supervisor 的 `censorguard.config`
一致、`censorpivot` 可读取 YAML，且可连接其中的 launch socket。注册失败会直接终止
runner，不输出 Ready，不执行工具。

`cargo test --test guard_launcher` 直接验证新版 runner 的注册握手，无需额外 launcher。

在 root/BPF LSM/mount namespace 主机上，可执行 `python3 scripts/test-guard-runtime.py`
验证非 root runner 的真实文件/exec 拒绝与审计，无需安装服务。它使用 Guard release
二进制、BPF 对象和 Pivot debug 二进制；不是完整 FUSE 事务验收。

## 8. Scope 维护交接与本次提交说明

上次 Scope 修改覆盖四点：私有 mount namespace 中的非 root runner 登记、短批次
track-remove 前事件落盘、daemon 重启后的 spool 去重 checkpoint，以及 DSH 常驻 worker
的模块身份/权限配置同步。具体文件、内部接口变化、安全授权边界和验证证据见
[Scope 修改交接说明](SCOPE-CHANGES.zh-CN.md)，可直接交给 Scope 维护者审查。

本次部署脚本与文档的英文 commit 文案：

```text
feat(pivot): add host setup and deployment workflow

- Prepare cgroup v2 and BPF LSM boot settings with backups
- Check kernel, build dependencies, FUSE, storage and cgroup lifecycle support
- Install and start FS, Guard, Scope and Pivot with optional DSH setup and demo
- Document deployment limits and hand off the Scope integration fixes
```
