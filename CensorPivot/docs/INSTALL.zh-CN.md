# CensorPivot 安装与运行

统一安装 CensorFS、CensorGuard、CensorScope、Pivot 和 DSH 组合插件。
以下命令在 **AgentCensor 仓库根目录**执行，示例路径按实际位置替换。

## 快速安装

准备 Linux 宿主机或虚拟机、root 可用的 Rust/rustup，以及 Node.js 22+、pnpm 和
已安装依赖的 DeepSeek Harness `0.1.5-rc.2`。缺少 Rust 时展开下方说明。
更新前先退出 DSH；已有独立组件 daemon 时，按下方说明停止它们。

### 1. 安装组件和插件

```bash
cd /root/AgentCensor
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh install --install-deps \
  --dsh-root /root/deepseek-harness
```

脚本依次构建安装 FS、Guard、Scope、Pivot，再安装 DSH 的 web/headless profiles。
`--install-deps` 安装系统构建依赖，依赖齐全时省略；不需要 DSH 时省略 `--dsh-root`。
**此步骤不启动服务，当前使用 cgroup v1 也能先安装。**

### 2. 准备运行环境

openEuler/RHEL 等使用 `grubby` 的主机执行：

```bash
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh prepare --configure-boot
```

- 输出环境已就绪：直接进入第 3 步，无需重启。
- 输出 `default kernel boot arguments verified` 和 `REBOOT REQUIRED`：
  引导参数已验证写入，在维护窗口执行一次 `sudo reboot`，再进入第 3 步。
- 输出错误：先解决错误，不要重启。

cgroup v2 和 BPF LSM 会一次配置，并备份原引导文件。退出码 **10** 表示等待重启生效，
不要用 `&& reboot` 串接。没有 `grubby` 的系统见下方手动配置说明。

### 3. 启动后台服务

重启后回到仓库目录，无需重新安装：

```bash
cd /root/AgentCensor
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh start
```

脚本检查运行条件，启动 `agentcensord.service` 和 `censorpivot.service`，并执行 doctor。
`agentcensord` 统一管理 FS、Guard、Scope；不要再单独启动这三个 daemon。

首次启动会将 `/var/lib/agentcensor/workspace` 导入为 FS 初始基线。
需要导入已有工程时，先把工程放入该目录；后续启动不会重复导入。

### 4. 打开 DSH Web

使用安装插件的同一用户执行；root 发起安装时使用 root：

```bash
export PATH="$HOME/.local/bin:$PATH"
cd /root/deepseek-harness
pnpm dsh web
```

`pnpm dsh --profile web` 与上面的命令等价。Web 连接已运行的后台服务，关闭 Web 后
后台服务继续运行；安装器已启用服务自启动。

**环境已就绪的机器**也可直接安装并启动：

```bash
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh run \
  --dsh-root /root/deepseek-harness
```

## 日常操作

以下命令在 AgentCensor 仓库根目录执行：

```bash
# 状态与诊断
sudo bash CensorPivot/scripts/install-agentcensor.sh status
sudo bash CensorPivot/scripts/install-agentcensor.sh doctor

# 停止 / 启动全部后台组件
sudo bash CensorPivot/scripts/install-agentcensor.sh stop
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh start

# 只更新 DSH 插件：先退出 DSH，安装后重新打开 Web
DSH_ROOT=/root/deepseek-harness bash CensorPivot/scripts/install-dsh-censorpivot.sh
```

更新源码后，先退出 DSH，再重复快速安装的第 1、3、4 步。
安装默认保留已有配置；`cargo build` 不会更新系统程序或已安装插件。
修改基础配置后，用上述 stop/start 重启全部组件，再打开 DSH；动态策略组使用
`censorguardctl policy apply`。

查看后台日志：

```bash
sudo journalctl -u agentcensord.service -u censorpivot.service -n 100 --no-pager
```

## 配置位置

| 路径 | 用途 |
|---|---|
| `/etc/agentcensor/censord.json` | 三个组件的启动配置：FS 存储/socket，Guard YAML 和 Scope 配置路径 |
| `/etc/censorguard/agentcensor.yaml` | Guard 开关、基础规则和连接配置；Pivot 共用此文件 |
| `/etc/censorscope/censorscoped.conf` | Scope 采集与存储配置 |
| `/etc/censorpivot/config.json` | Pivot 批次、组件连接、策略组白名单和事务存储 |

统一启动会核对配置连接关系，不一致时拒绝启动。Guard 动态策略组单独持久化，重启后恢复。
如需恢复仓库示例，先审查差异，再使用安装器的 `--force-config`；覆盖前会保存 `.bak`。

## 分步说明与排障

<details>
<summary>缺少 Rust：安装管理器</summary>

已有 root rustup 时跳过。安装器会识别 `/root/.cargo/bin`，按各项目固定版本选择工具链。
缺少的工具链会自动下载。

```bash
curl --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 --retry 2 -sSf \
  https://sh.rustup.rs -o /tmp/agentcensor-rustup-init.sh
sudo -H timeout 300 sh /tmp/agentcensor-rustup-init.sh \
  -y --profile minimal --default-toolchain none
```

停在 `downloading installer` 时，检查 `static.rust-lang.org` 的网络/代理，或使用已确认的
镜像；这与 cgroup、重启无关。按 Ctrl+C 中断不代表安装成功。

</details>

<details>
<summary>检查环境、手动配置引导和重启问题</summary>

构建要求 Rust、clang 17+ 及 BPF 后端、llvm-strip、bpftool、libbpf/libelf/zlib 开发包和 BTF。
完整运行要求 systemd、可写 cgroup v2（支持 `cgroup.kill`）、FUSE、BTF、BPF LSM，
FS 存储需位于本地 ext4/XFS。

```bash
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh check --build-only
sudo -H bash CensorPivot/scripts/setup-agentcensor.sh check
```

`prepare` 不带 `--configure-boot` 时只检查，不修改引导。配置成功也要等下一次启动生效。
如果重启后仍不满足条件，比对以下输出，不要反复重启：

```bash
uname -r
cat /proc/cmdline
sudo grubby --info=DEFAULT
stat -fc %T /sys/fs/cgroup
cat /sys/kernel/security/lsm
```

预期：实际引导参数与计划一致；cgroup 文件系统为 `cgroup2fs`；LSM 列表包含 `bpf`。
默认引导项有新参数、`/proc/cmdline` 没有时，检查实际选择的引导项。
旧脚本曾因 grubby 同时删除/添加同名参数误报成功；新版会读回验证，未写入时拒绝提示重启。

Debian/Ubuntu 没有 `grubby` 时，备份并编辑 `/etc/default/grub`：

```bash
sudo cp -a /etc/default/grub /etc/default/grub.agentcensor.bak
sudoedit /etc/default/grub
```

保留 `GRUB_CMDLINE_LINUX` 的其他参数，设置
`systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all`；保留已有 `lsm=` 列表并追加 `bpf`，
未显式设置时参考当前 LSM 列表。避免重复的同名参数，然后执行 `sudo update-grub`，
成功后重启一次。不要覆盖挂载运行中的 cgroup v1；容器需要宿主提供可写 cgroup v2。

</details>

<details>
<summary>独立组件冲突、细分安装和旧配置升级</summary>

已有独立 daemon 时，只对实际存在的组件执行停止命令，再进行统一安装：

```bash
sudo systemctl disable --now censorguardd.service
sudo systemctl disable --now censorfsd.service
sudo /usr/sbin/censorscoped stop
```

统一安装内部依次执行以下步骤，可用于单独更新某个组件：

```bash
sudo -H bash CensorPivot/scripts/install-agentcensor.sh fs
sudo -H bash CensorPivot/scripts/install-agentcensor.sh guard
sudo -H bash CensorPivot/scripts/install-agentcensor.sh scope
sudo -H bash CensorPivot/scripts/install-agentcensor.sh pivot
```

`pivot` 创建服务用户、systemd 单元、sudoers 和配置，并迁移旧 Guard 接口配置；迁移备份为
`.pre-guard-0.5.0.bak`。配置中的未知选项或连接冲突需先解决。
`--force-config pivot` 替换 Pivot/supervisor JSON，`--force-config guard` 替换 Guard 示例配置。

Guard 的共用 YAML 必须能被 `censorpivot` 用户读取；默认目录/socket 组为 `censorpivot`。
自定义路径也需检查目录遍历权限。Scope daemon 应运行在 Guard 受管树外，统一 supervisor
已采用此方式；加入 Guard 组不能豁免 ptrace/BPF 拦截。

</details>

<details>
<summary>DSH 插件检查与可选设置</summary>

在 DSH 源码目录执行：

```bash
pnpm dsh plugin --profile web list --depth 0
pnpm dsh --profile web --dump-config
```

插件安装默认重建并打包当前源码。使用 nvm 时，确保安装用户的登录 shell 能找到 Node/pnpm。
Web 与 headless worker 使用同一 DSH 源码目录。

| 设置 | 用途 |
|---|---|
| `DSH_ROOT` | DSH 源码目录 |
| `DSH_BIN` | 可执行的 DSH 命令；有源码时优先用 `DSH_ROOT` |
| `DSH_PNPM_STORE_DIR` | 指定可写的 pnpm store |
| `CENSORPIVOT_PACKAGE_DIR` | 复用预构建组合包；默认从源码重建 |

</details>

可选原生事务验证：`setup-agentcensor.sh start --demo`，会向 FS `main` 发布测试结果。
当前 DSH 普通对话尚未自动提交 Pivot `BatchRequest`；完整事务接入范围见
[验收报告](ACCEPTANCE.zh-CN.md)，Scope 改动见[维护交接说明](SCOPE-CHANGES.zh-CN.md)。
