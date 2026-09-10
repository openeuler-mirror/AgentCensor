# openEuler 鲲鹏构建与部署

本文面向普通鲲鹏/AArch64 服务器，说明 CensorFS 在 openEuler 24.03 LTS、Linux 6.6 或 6.12 上的依赖、构建、安装和验证方式。示例中的 `/data` 代表本地 XFS 或 ext4 数据盘挂载点，可按实际部署规划替换。

## 1. 支持矩阵

| 项目 | 支持范围 |
|---|---|
| CPU 架构 | AArch64（包括鲲鹏） |
| 操作系统 | openEuler 24.03 LTS |
| Linux 内核 | 6.6、6.12；最低版本 6.6 |
| Rust 工具链 | 1.82.0 |
| 持久化文件系统 | 同一块本地 XFS 或 ext4 |
| FUSE | 内核 FUSE 和 `/dev/fuse` |

CensorFS 使用通用 Linux 系统调用，不包含鲲鹏专用业务逻辑。AArch64 是正式交付目标；为 x86-64 构建时需要生成独立机器码，但两种架构可以读取同一磁盘格式。

## 2. 依赖

一键脚本安装下列 openEuler 软件包：

| 软件包 | 用途 | 运行时是否必需 |
|---|---|---:|
| `gcc`, `gcc-c++`, `make` | Rust 依赖中的本地代码编译和链接 | 否 |
| `pkgconf-pkg-config` | 构建时依赖发现 | 否 |
| `git`, `curl`, `ca-certificates` | 获取源码依赖和 Rust 工具链 | 否 |
| `tar`, `gzip`, `findutils` | 工具链与构建脚本 | 否 |
| `util-linux` | Namespace、`mount`、`unshare` 等系统工具 | 是 |
| `fuse3` | FUSE 用户态工具 | 真实 FUSE 数据面需要 |
| `jq` | openEuler smoke 测试解析控制接口输出 | 仅测试需要 |

`fuser 0.15.1` 已固定在仓库的 `vendor/fuser`，并关闭其 libfuse 默认特性，因此编译本项目不依赖 `fuse3-devel`。其余 Rust crate 版本由 `Cargo.lock` 固定，首次在线构建会从 crates.io 下载。

## 3. 构建前检查

```bash
uname -m
uname -r
. /etc/os-release
printf '%s\n' "$PRETTY_NAME"
```

预期：

- `uname -m` 输出 `aarch64`；
- 系统为 openEuler 24.03；
- 内核主次版本不低于 6.6。

选择持久化目录前检查 backing store：

```bash
mkdir -p /data/censorfs
stat -f -c %T /data/censorfs
```

输出应为 `xfs` 或 `ext2/ext3`；Linux 的 `stat` 使用 `ext2/ext3` 名称表示 ext 系列文件系统，其中包括 ext4。不要把 `.censorfs` 放在 NFS、CIFS、OverlayFS、tmpfs 或另一个 FUSE 文件系统上。

真实 FUSE 数据面还应检查：

```bash
sudo modprobe fuse
test -c /dev/fuse
```

## 4. 一键构建

在源码根目录执行：

```bash
bash scripts/build-openeuler-aarch64.sh --install-deps
```

非 root 用户执行时，脚本通过 `sudo dnf` 安装依赖；如果依赖已由管理员安装，可以省略 `--install-deps`：

```bash
bash scripts/build-openeuler-aarch64.sh
```

可用选项：

| 选项 | 作用 |
|---|---|
| `--install-deps` | 使用 `dnf` 安装构建、运行和 smoke 测试依赖 |
| `--with-tests` | release 构建前执行 workspace 全部测试 |
| `--offline` | Cargo 离线模式；要求 Rust 工具链和 Cargo 缓存已经准备完毕 |
| `--help` | 显示帮助 |

`--offline` 与 `--install-deps` 不能同时使用；离线服务器应先由管理员从内部 RPM 源准备依赖。

带完整测试的构建：

```bash
mkdir -p /data/censorfs-test-tmp
CENSORFS_TEST_TMPDIR=/data/censorfs-test-tmp \
  bash scripts/build-openeuler-aarch64.sh --with-tests
```

`CENSORFS_TEST_TMPDIR` 必须位于本地 XFS/ext4。测试会把它作为 `TMPDIR`，从而让涉及持久化目录校验的用例运行在受支持的 backing store 上。

构建结果位于：

```text
target/release/censorfs
target/release/censorfsd
target/release/censorfsctl
target/release/censorfs-mounter
```

脚本还会在同一目录创建 `censorfs-init`、`censorfs-info`、`censorfs-ls` 等符号链接，它们全部指向 `censorfs`。

## 5. 手动构建

需要定制构建流程时，可以执行：

```bash
sudo dnf install -y \
  gcc gcc-c++ make pkgconf-pkg-config git curl ca-certificates \
  tar gzip findutils util-linux fuse3 jq

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain 1.82.0
. "$HOME/.cargo/env"

rustup toolchain install 1.82.0 --profile minimal
cargo +1.82.0 build --workspace --release --locked
bash scripts/install-censorfs-links.sh "$PWD/target/release"
```

需要先运行测试时：

```bash
mkdir -p /data/censorfs-test-tmp
TMPDIR=/data/censorfs-test-tmp \
  cargo +1.82.0 test --workspace --all-targets --locked
```

## 6. 离线构建

`--offline` 不会下载 Rust 工具链或 crate。离线服务器必须预先具备：

1. Rustup 与 Rust 1.82.0 AArch64 工具链；
2. `Cargo.lock` 中所有依赖的 Cargo registry/cache；
3. 第 2 节列出的 RPM 软件包；
4. 完整源码，包括 `vendor/fuser`。

联网的同版本 AArch64 构建机可先执行：

```bash
rustup toolchain install 1.82.0 --profile minimal
cargo +1.82.0 fetch --locked --target aarch64-unknown-linux-gnu
```

将 Rust 工具链和 Cargo 缓存按组织的软件供应链流程同步到离线服务器后执行：

```bash
bash scripts/build-openeuler-aarch64.sh --offline
```

生产环境建议把 Rust 工具链、crate 缓存和 RPM 依赖纳入内部制品库，并在发布流水线中保留 `Cargo.lock` 和构建日志。

## 7. 安装可执行文件

下面给出一种通用布局：底层服务程序放在 `/usr/libexec/censorfs`，可读命令放在 `/usr/local/bin`。

```bash
sudo install -d -m 0755 /usr/libexec/censorfs
sudo install -m 0755 \
  target/release/censorfsd \
  target/release/censorfsctl \
  target/release/censorfs-mounter \
  /usr/libexec/censorfs/

sudo install -m 0755 target/release/censorfs /usr/local/bin/censorfs
sudo bash scripts/install-censorfs-links.sh /usr/local/bin
```

如果调度器直接调用 `censorfs-mounter`，请使用固定绝对路径，并通过 sudoers、systemd 或容器运行时只授予可信调度器所需权限。不要让普通 Agent 获得 `.censorfs` 的读写权限。

## 8. 初始化和 systemd 部署

创建服务组和数据目录：

```bash
getent group censorfs >/dev/null || sudo groupadd --system censorfs
sudo install -d -o root -g censorfs -m 0750 /var/lib/censorfs
sudo install -d -o root -g root -m 0755 /data/censorfs-base
```

把初始工作区内容放入 `/data/censorfs-base`，然后只初始化一次：

```bash
sudo /usr/libexec/censorfs/censorfsctl init \
  --storage-root /var/lib/censorfs/.censorfs \
  --import-root /data/censorfs-base \
  --branch main
```

确认 `/var/lib/censorfs` 自身位于本地 XFS/ext4。安装仓库提供的服务单元：

```bash
sudo install -m 0644 deploy/systemd/censorfsd.service \
  /usr/lib/systemd/system/censorfsd.service
sudo systemctl daemon-reload
sudo systemctl enable --now censorfsd
sudo systemctl status censorfsd
```

服务默认使用：

| 项目 | 路径 |
|---|---|
| 持久化实例 | `/var/lib/censorfs/.censorfs` |
| 控制 Socket | `/run/censorfs/control.sock` |
| 服务程序 | `/usr/libexec/censorfs/censorfsd` |

控制接口 Socket 权限为 `0660`。需要运行 `censorfsctl` 的受信用户可以加入 `censorfs` 组；生产授权仍由 daemon 读取的 `SO_PEERCRED` UID/GID 决定。

## 9. 验证

### 9.1 可读核心场景

```bash
export PATH="$PWD/target/release:$PATH"
CENSORFS_DEMO_PARENT=/data bash scripts/demo-two-branch-isolation.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-merge.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-abort.sh
CENSORFS_DEMO_PARENT=/data bash scripts/demo-crash-fsck.sh
```

这些脚本通过 Unix Socket 操作 `ViewEngine`，验证多分支隔离、Merge、Abort 和崩溃恢复，不创建真实 FUSE mount。

### 9.2 真实 FUSE 和 Namespace

先完成第 4 节的 release 构建，再执行：

```bash
sudo CENSORFS_TEST_PARENT=/data bash scripts/openeuler-real-smoke.sh
```

该脚本会在本地 XFS/ext4 测试目录内完成：

- workspace 初始内容读取；
- Ticket View 中的真实 FUSE 写入；
- Prepare 和 Publish；
- 只读稳定 View 验证；
- 独立 Mount Namespace 生命周期检查。

多 View、独立分支发布和回滚测试应由非 root Agent 用户执行，该用户需要被明确授权以 root 运行 `censorfs-mounter`：

```bash
CENSORFS_TEST_PARENT=/data bash scripts/openeuler-multiview-smoke.sh
```

### 9.3 fsck 运维规则

`censorfs-fsck` 必须在 daemon 停止后运行：

```bash
sudo systemctl stop censorfsd
sudo CENSORFS_STORAGE_ROOT=/var/lib/censorfs/.censorfs censorfs-fsck
sudo CENSORFS_STORAGE_ROOT=/var/lib/censorfs/.censorfs censorfs-fsck --repair
sudo systemctl start censorfsd
```

默认检查不修改数据。`--repair` 只执行可证明安全的恢复，不修复损坏 Object、不回拨 Branch Head，也不删除孤立 Object。

## 10. 上线前验收

至少在目标机型和实际数据盘上完成：

- Linux 6.6 或 6.12 原生 release 构建和完整测试；
- 本地 XFS/ext4 上的真实 FUSE、Namespace、UID/GID 与只读视图测试；
- 多分支并发发布、Merge 冲突、Abort 和回滚测试；
- daemon `SIGKILL` 后的离线检查、修复和重启；
- 物理断电、写缓存、介质错误、容量耗尽和长时间压力测试；
- systemd 自动启动、日志、权限、备份、监控和版本升级演练。

故障注入和 `SIGKILL` 测试验证软件恢复协议，但不能替代物理断电认证。
