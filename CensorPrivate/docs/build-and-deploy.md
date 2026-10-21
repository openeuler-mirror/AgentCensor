# Rust 0.3.0 构建与部署教程

本文适用于 CensorPrivate 0.3.0，从源码构建开始，依次完成 release 安装、本机验证和 systemd 部署。示例面向 Linux；普通用户完成构建，安装系统软件和系统服务时使用 `sudo`。

日常操作推荐使用 [Rust 部署与使用手册](usage.md) 中的 `cpctl` 短命令。本文保留底层命令，用于理解配置和排查问题。两条路线分别使用 `managed` / `rust` 配置目录及 8775 / 8765 端口，请选定一条路线，不要交叉使用 token 或 run 文件。

阅读路线：

- [1. 版本与目录约定](#1-版本与目录约定)
- [2. 准备操作系统与源码](#2-准备操作系统与源码)
- [3. 构建与安装](#3-构建与安装)
- [4. 在本机启动网关](#4-在本机启动网关)
- [5. 验证安装、认证和模型连通性](#5-验证安装认证和模型连通性)
- [6. 部署为 systemd 服务](#6-部署为-systemd-服务)
- [7. 接入工具和可选运行模式](#7-接入工具和可选运行模式)
- [8. 策略更新、版本更新与回退](#8-策略更新版本更新与回退)
- [9. 常见问题](#9-常见问题)

## 1. 版本与目录约定

- 源码：项目已发布的 Rust 版本，需要 Rust ≥ 1.85 和 C 链接器。
- 构建产物：`target/release/` 下的五个命令，以及可选 Linux 发布归档。
- 本机端口：`8765`；配置目录：`$HOME/.config/censorprivacy/rust`。
- 系统安装：`/usr/local/bin/`。
- systemd 服务：`censorprivacy@model-rust`。

项目构建、测试和运行均不依赖 Python。架构与安全设计见 [架构说明](architecture.md)，测试入口与结果见 [验收记录](validation.md)。

本项目提供 `censorprivacy`、`privacy-run`、`privcalc`、`censorprivacy-mcp`、`tls-run` 五个命令。普通网关流程是：客户端使用本机 token → 本地策略将敏感内容替换为 CP 引用 → 网关使用 provider key 请求上游 → 回复在授权的被动显示位置恢复。SECRET 原值不恢复。

一个服务实例对应一个 run。原值与引用映射只在服务内存中；`run.json` 仅记录 run ID，不能恢复映射。私密计算的派生结果也以引用返回，避免把结果重新泄漏给模型。

## 2. 准备操作系统与源码

### 2.1 安装基础工具

按发行版选择一组命令。

Ubuntu / Debian：

```bash
sudo apt-get update
sudo apt-get install -y git build-essential pkg-config ca-certificates curl jq openssl
```

openEuler / RHEL 系：

```bash
sudo dnf install -y git gcc make pkgconf-pkg-config ca-certificates curl jq openssl
```

Rust 使用 1.85 或更高版本，验收使用 1.85.1。通过发行版软件源或 [Rust 官方安装说明](https://www.rust-lang.org/tools/install) 安装工具链。已有 rustup 时，可安装验收使用的版本：

```bash
rustup toolchain install 1.85.1 --component rustfmt --component clippy
rustc +1.85.1 --version
cargo +1.85.1 --version
cc --version
```

普通网关使用 Rustls，不需要 OpenSSL 开发库；上面的 `openssl` 命令用于生成本机 token。TLS direct 的额外依赖在第 7 节单独安装。

### 2.2 确认源码分支

在当前仓库根目录执行：

```bash
git branch --show-current
CP_RUST_SRC="$(pwd -P)"
test -f "$CP_RUST_SRC/Cargo.toml"
test -f "$CP_RUST_SRC/Cargo.lock"
```

确认检出的版本与部署目标一致，目录中应有 `Cargo.toml`、`Cargo.lock`、`src/`、`crates/` 和 `scripts/`。后续 `CP_RUST_SRC` 始终是该目录的绝对路径，换终端或构建主机后要重新设置。源码目录与 agent 的可写 workspace 应分别管理。

这些 shell 变量只在设置它们的终端中生效。新终端中若未重新设置，`"$CP_RUST_SRC/native"` 会展开成 `/native`；已经位于仓库根目录时，可以直接使用 `make -C native`。

## 3. 构建与安装

### 3.1 生成 release 可执行文件

```bash
cd "$CP_RUST_SRC"
rustup override set 1.85.1
cargo fetch --locked
cargo build --locked --release --bins
target/release/censorprivacy --version
target/release/censorprivacy serve --help
```

`override` 只为该源码目录选择工具链，供构建脚本中的 Cargo 使用。若已使用符合要求的系统 Rust，可省略该行。`--locked` 要求遵循仓库中的 `Cargo.lock`，依赖变化时不会悄悄更新锁文件。首次构建会下载依赖并编译，需预留数 GB 的磁盘空间。

成功后，`target/release/` 下有五个命令，主程序版本为 `0.3.0`。可直接使用这些路径，也可以安装到当前用户目录：

```bash
bash scripts/install.sh --prefix "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

安装脚本会执行 release 构建并复制产物。构建过程使用普通用户；第 6 节再用 `sudo install` 部署已经生成的文件。Rust 产品运行不需要 Python。

### 3.2 生成可传输的发布包

```bash
cd "$CP_RUST_SRC"
bash scripts/package.sh
CP_PACKAGE="censorprivacy-0.3.0-linux-$(uname -m)"
sha256sum -c "dist/$CP_PACKAGE.tar.gz.sha256"
tar -tzf "dist/$CP_PACKAGE.tar.gz"
```

成功标志是校验显示 `OK`，归档内包含 `usr/local/bin/`、策略示例和 systemd 模板。加 `--with-native` 可以包含 TLS direct 所需的 C 库，先按第 7 节安装其编译依赖。

校验文件中的路径带有 `dist/`。传到另一台机器时保留 `dist/` 目录，并在它的父目录运行 `sha256sum -c dist/文件名.sha256`。归档应先解压到暂存目录，再按第 6 节的文件布局安装。

该归档是目标架构的 Linux 构建，并非静态跨平台包；部署机器应具有兼容的 libc 和系统动态库。换架构或较旧的系统时，应在兼容目标的构建环境重新编译。

## 4. 在本机启动网关

网关需要可达的上游才能返回模型回复，上游可以部署在本机、内网或云端。仅验证构建与处理链路时，可以直接执行第 5.3 节的本机模拟验收，无需部署模型，也无需准备 provider key。

| 测试目标 | 使用的上游 | 是否需要真实模型或云账号 |
|---|---|---|
| 检查启动、认证和文件权限 | `/ready`、`/v1/models` 不请求上游 | 不需要 |
| 检查保护、转发、恢复和 CLI | 第 5.3 节测试自动启动本机模拟服务 | 不需要 |
| 获取真实推理结果 | 本机、内网或云端兼容模型服务 | 需要可用模型服务；云账号取决于所选服务 |

### 4.1 设置路径和端口

```bash
CP_BIN="$CP_RUST_SRC/target/release/censorprivacy"
CP_PORT=8765
CP_CONTROL_DIR="$HOME/.config/censorprivacy/rust"
```

此处直接使用 release 路径，避免 PATH 指向其他安装位置。也可将 `CP_BIN` 设为已经安装的 Rust 可执行文件绝对路径。

### 4.2 首次初始化策略和凭据

本段只在首次初始化时执行；后续启动复用已有文件。`CP_CONTROL_DIR` 必须位于 agent workspace 和可见依赖树之外。

下面的 provider key 输入适用于需要上游认证的模型服务。无需认证的本机服务可跳过该文件准备，并在启动命令中省略 `--upstream-key-file`；本机 gateway token 仍然必需。

```bash
umask 077
install -d -m 0700 "$CP_CONTROL_DIR"
install -m 0600 "$CP_RUST_SRC/examples/policy.yaml" "$CP_CONTROL_DIR/policy.yaml"
openssl rand -hex 32 > "$CP_CONTROL_DIR/gateway.token"
read -r -s -p '输入 provider key: ' CP_PROVIDER_KEY
printf '\n'
printf '%s\n' "$CP_PROVIDER_KEY" > "$CP_CONTROL_DIR/upstream.key"
unset CP_PROVIDER_KEY
printf 'Authorization: Bearer %s\n' "$(cat "$CP_CONTROL_DIR/gateway.token")" \
  > "$CP_CONTROL_DIR/curl.headers"
chmod 0600 "$CP_CONTROL_DIR/gateway.token" "$CP_CONTROL_DIR/upstream.key" "$CP_CONTROL_DIR/curl.headers"
```

随机 token 用于客户端访问本机网关，provider key 用于网关访问模型服务，二者不能混用。provider key 通过无回显输入写入文件，不写在 shell 命令历史中。`curl.headers` 也包含本机 token，按凭据保管。

检查权限，不要输出凭据内容：

```bash
stat -c '%a %n' "$CP_CONTROL_DIR" "$CP_CONTROL_DIR/gateway.token" "$CP_CONTROL_DIR/upstream.key"
```

目录应为 `700`，凭据应为 `600`。Rust 版拒绝使用组或其他用户可读的私密文件，并要求 run 状态文件的父目录私有。

复制的策略只是起点：金额、日期等内置规则已启用，组织名称、客户和业务秘密需要填写词典或正则。可以在 `policy.yaml` 的 `dictionaries` 中加入演示规则，例如用以下内容替换原来的 `dictionaries: {}`：

```yaml
dictionaries:
  CUSTOMER:
    - 演示客户甲
```

### 4.3 连接实际模型服务并前台启动

本段适用于已部署或已开通的模型服务。提示中的 `.example` 域名都是占位地址，不能直接用于发送模型请求；`--model` 必须填写所选上游实际支持的模型 ID。

```bash
read -r -p '输入上游 origin（例如 https://model.example）: ' CP_UPSTREAM
read -r -p '输入该上游可用的模型 ID: ' CP_MODEL
"$CP_BIN" serve \
  --policy "$CP_CONTROL_DIR/policy.yaml" \
  --upstream "$CP_UPSTREAM" \
  --model "$CP_MODEL" \
  --token-file "$CP_CONTROL_DIR/gateway.token" \
  --upstream-key-file "$CP_CONTROL_DIR/upstream.key" \
  --run-state-file "$CP_CONTROL_DIR/run.json" \
  --host 127.0.0.1 --port "$CP_PORT"
```

上游配置填写实际服务的 origin，网关会追加 `/v1/chat/completions` 等路径；不要在 origin 后重复加 `/v1`。上游需要支持本项目实现的协议，兼容服务的自定义路径前缀应按其实际路由设置。

若本机已经有兼容 Chat Completions 的 HTTP 模型服务，例如监听 `127.0.0.1:8000`，可使用下面的启动形式。该命令不会替你启动模型服务；先把 `LOCAL_MODEL_ID` 换成其实际模型 ID。示例按上游无需认证配置，本机网关仍要求 token：

```bash
"$CP_BIN" serve \
  --policy "$CP_CONTROL_DIR/policy.yaml" \
  --upstream http://127.0.0.1:8000 \
  --allow-http-test-upstream \
  --model LOCAL_MODEL_ID \
  --token-file "$CP_CONTROL_DIR/gateway.token" \
  --run-state-file "$CP_CONTROL_DIR/run.json" \
  --host 127.0.0.1 --port "$CP_PORT"
```

`--allow-http-test-upstream` 只允许回环地址的 HTTP 上游，不能用它开放远程明文上游。远程或内网模型服务使用 HTTPS；有认证要求时再提供 `--upstream-key-file`。

终端留在前台且进程没有报错退出后，在另一个终端验证。服务默认不打印请求正文，也不靠启动日志判断就绪。按 `Ctrl+C` 可停止前台服务。

如上游使用私有 CA，增加 `--upstream-ca-file /绝对路径/ca.pem`。本机 HTTPS 入口同时提供 `--tls-cert` 和 `--tls-key`，私钥权限为 `0600`；客户端仍需校验证书。下面的验证示例采用本机 HTTP，`privacy-run` 和 `tls-run` 的 readiness 控制面也要求本机 HTTP。

## 5. 验证安装、认证和模型连通性

### 5.1 在第二个终端检查本机服务

在第二个终端设置本教程的端口和目录：

```bash
CP_PORT=8765
CP_CONTROL_DIR="$HOME/.config/censorprivacy/rust"
umask 077
curl --noproxy '*' --fail --silent --show-error \
  --header "@$CP_CONTROL_DIR/curl.headers" \
  "http://127.0.0.1:$CP_PORT/ready" > "$CP_CONTROL_DIR/ready.json"
jq . "$CP_CONTROL_DIR/ready.json"
curl --noproxy '*' --fail --silent --show-error \
  --header "@$CP_CONTROL_DIR/curl.headers" \
  "http://127.0.0.1:$CP_PORT/v1/models" > "$CP_CONTROL_DIR/models.json"
jq . "$CP_CONTROL_DIR/models.json"
```

`/ready` 应返回 `status: "ready"`、版本 `0.3.0` 和 `run_id`。`/v1/models` 仅列出启动时通过 `--model` 配置的模型，不向上游查询，也不验证该模型名是否存在。两个入口都要求本机 token；它们成功只证明本机服务已就绪，并未验证 provider key 或上游可达性。

不带认证再访问一次，预期 HTTP 状态码为 `401`：

```bash
curl --noproxy '*' --silent --show-error --output /dev/null \
  --write-out '%{http_code}\n' "http://127.0.0.1:$CP_PORT/ready"
stat -c '%a %n' "$CP_CONTROL_DIR/run.json"
```

状态文件应为 `600`。示例使用文件传递认证头，避免把 token 展开到 curl 的进程命令行；`--noproxy '*'` 保证本机探测不经过环境中的 HTTP 代理。

### 5.2 发出一次真实模型请求

下面适用于支持 Chat Completions 的上游。它使用模型列表中的第一个 ID，并发送无敏感数据的连通性消息：

```bash
CP_MODEL="$(jq -er '.data[0].id' "$CP_CONTROL_DIR/models.json")"
jq -n --arg model "$CP_MODEL" \
  '{model: $model, messages: [{role: "user", content: "请回复：网关连接成功。"}], stream: false}' \
  > "$CP_CONTROL_DIR/request.json"
curl --noproxy '*' --fail --silent --show-error \
  --header "@$CP_CONTROL_DIR/curl.headers" \
  --header 'Content-Type: application/json' \
  --data-binary "@$CP_CONTROL_DIR/request.json" \
  "http://127.0.0.1:$CP_PORT/v1/chat/completions"
```

成功返回协议允许的模型回复，才说明网关到上游的链路可用。若上游只提供 Messages 或 Responses，应使用对应的请求格式和路径。客户端配置对应关系如下：

| 客户端协议 | 本机入口 | 通常配置的 base URL |
|---|---|---|
| Chat Completions | `/v1/chat/completions` | `http://127.0.0.1:端口/v1` |
| Responses | `/v1/responses` | `http://127.0.0.1:端口/v1` |
| Messages | `/v1/messages` | `http://127.0.0.1:端口`，按客户端路径拼接方式调整 |

客户端 API key 填本机 gateway token，provider key 留在服务端。

如果 curl 只报告 `502`，可去掉 `--fail` 并打印响应状态，查看网关的固定错误码：

```bash
curl --noproxy '*' --silent --show-error \
  --header "@$CP_CONTROL_DIR/curl.headers" \
  --header 'Content-Type: application/json' \
  --data-binary "@$CP_CONTROL_DIR/request.json" \
  --write-out '\nHTTP %{http_code}\n' \
  "http://127.0.0.1:$CP_PORT/v1/chat/completions"
```

`upstream_unavailable` 通常表示 DNS、连接或证书校验失败；`upstream_status` 表示上游返回了非成功状态，需核对服务路径、凭据、模型 ID 和上游自身状态；`upstream_timeout` 表示请求超时。网关不会回传上游原始错误正文。将占位地址 `https://api.provider.example` 当作服务地址会导致实际转发失败，readiness 仍可能成功。

### 5.3 验证隐私行为

如果只想验收安装所依赖的 Rust 实现和 CLI，可以直接在仓库根目录运行下面的本机测试，不必先完成第 4 节的上游配置。测试自带模拟上游、虚构数据、临时凭据和临时端口；不会读取正式 provider key，不会请求云模型，也不会修改当前运行网关的配置：

```bash
cd "$CP_RUST_SRC"
cargo test --locked --test http_gateway --test https_cli
```

成功标志是两组测试均通过。`http_gateway` 验证三个协议的保护与转发、认证拒绝、SSE 和私密计算；`https_cli` 启动实际 Rust 二进制与本机 HTTPS 模拟上游，验证证书、状态文件、策略重载和 `privcalc`。HTTPS 测试需要系统的 `openssl` 命令，依赖已在第 2 节安装。首次编译可能访问 Cargo 包源下载依赖，这与调用模型服务无关。

这里验证的是 Cargo 当前构建的产物；系统安装路径是否正确，仍按第 6 节的 systemd 状态和认证 readiness 检查。需要完整常规回归时运行：

```bash
cd "$CP_RUST_SRC"
bash scripts/test/run-all.sh
```

自建模拟上游时，使用虚构敏感词检查上游实际收到的正文是否为引用。授权显示回复可能恢复原文，因此本机最终显示原文不能单独作为泄漏证据。

207 组固定核心样例由 Rust 常规测试直接验证，可单独执行：

```bash
cargo test --locked --test detection_fixtures --test compute_fixtures
```

真实 native 和 root 运行域测试是另行启用的扩展验收，详见 [验收记录](validation.md)。

## 6. 部署为 systemd 服务

本节使用专用非 root 账户运行网关。构建目录可以位于用户 home 下，服务执行文件安装到 `/usr/local`，以适配 `ProtectHome=yes`。正式配置放在 `/etc`，状态放在 `/var/lib`。

### 6.1 创建账户与目录

```bash
getent group censorprivacy >/dev/null || sudo groupadd --system censorprivacy
id -u censorprivacy >/dev/null 2>&1 || sudo useradd \
  --system --gid censorprivacy --home-dir /var/lib/censorprivacy \
  --no-create-home --shell /sbin/nologin censorprivacy
sudo install -d -o root -g censorprivacy -m 0750 /etc/censorprivacy
sudo install -d -o censorprivacy -g censorprivacy -m 0700 /var/lib/censorprivacy
```

专用服务账户读取凭据并维护本机 vault；运行 agent 使用另外的非 root 身份和专属 workspace。目录 `0700` 满足 Rust 对 run 状态父目录的要求。

### 6.2 安装系统文件

普通用户生成暂存文件，再由管理员只复制产物：

```bash
cd "$CP_RUST_SRC"
CP_STAGE="$(mktemp -d)"
DESTDIR="$CP_STAGE" bash scripts/install.sh --prefix /usr/local
sudo install -d -o root -g root -m 0755 /usr/local/bin /usr/local/share/censorprivacy
for CP_COMMAND in censorprivacy privacy-run privcalc censorprivacy-mcp tls-run; do
  sudo install -o root -g root -m 0755 \
    "$CP_STAGE/usr/local/bin/$CP_COMMAND" "/usr/local/bin/$CP_COMMAND"
done
sudo install -o root -g root -m 0644 \
  "$CP_STAGE/usr/local/share/censorprivacy/policy.example.yaml" \
  /usr/local/share/censorprivacy/policy.example.yaml
sudo install -o root -g root -m 0644 \
  deploy/censorprivacy@.service /etc/systemd/system/censorprivacy@.service
/usr/local/bin/censorprivacy --version
```

这里的 `DESTDIR` 是暂存根目录，`--prefix` 是目标安装前缀。若使用归档，解压后的包根目录可以替代 `CP_STAGE`；只复制可信并已验证的产物。部署完成后，暂存目录可由构建用户自行清理。

### 6.3 安装实例配置

使用本机测试已验证的真实上游和模型 ID：

```bash
CP_PORT=8765
CP_INSTANCE=model-rust
CP_SERVICE=censorprivacy@model-rust
CP_CONTROL_DIR="$HOME/.config/censorprivacy/rust"
read -r -p '输入已验证的上游 origin: ' CP_UPSTREAM
read -r -p '输入已验证的模型 ID: ' CP_MODEL
CP_ENV_FILE="$(mktemp)"
printf 'CP_UPSTREAM=%s\nCP_BIND=127.0.0.1\nCP_PORT=%s\nCP_MODEL=%s\n' \
  "$CP_UPSTREAM" "$CP_PORT" "$CP_MODEL" > "$CP_ENV_FILE"
sudo install -o root -g censorprivacy -m 0640 \
  "$CP_ENV_FILE" "/etc/censorprivacy/$CP_INSTANCE.env"
sudo install -o root -g censorprivacy -m 0640 \
  "$CP_CONTROL_DIR/policy.yaml" "/etc/censorprivacy/$CP_INSTANCE.policy.yaml"
sudo install -o censorprivacy -g censorprivacy -m 0600 \
  "$CP_CONTROL_DIR/gateway.token" "/etc/censorprivacy/$CP_INSTANCE.token"
sudo install -o censorprivacy -g censorprivacy -m 0600 \
  "$CP_CONTROL_DIR/upstream.key" "/etc/censorprivacy/$CP_INSTANCE.upstream-key"
```

`.env` 只保存公开配置；凭据独立为服务账户可读的 `0600` 普通文件。服务从 `/usr/local/bin/censorprivacy` 启动，run 状态写入 `/var/lib/censorprivacy/model-rust.run.json`。

复制模板后若增加自定义 CA、本机 HTTPS 或其他参数，修改对应 unit 的 `ExecStart`，并执行 `daemon-reload`。自定义安装前缀也需要同步修改该路径。

### 6.4 启动、验收与停止

先用 `Ctrl+C` 停止占用同一端口的前台服务，再执行：

```bash
sudo systemctl daemon-reload
sudo systemctl start "$CP_SERVICE"
sudo systemctl status "$CP_SERVICE" --no-pager
sudo journalctl -u "$CP_SERVICE" -n 50 --no-pager
sudo stat -c '%U %a %n' /var/lib/censorprivacy \
  "/var/lib/censorprivacy/$CP_INSTANCE.run.json"
```

成功标志是服务 `active (running)`，状态目录属于 `censorprivacy` 且为 `700`，run 文件为 `600`。复用第 5 节的认证头再次检查 `/ready` 和模型请求。systemd 启动的是新 run，其 ID 应以新的 readiness 为准。

需要开机启动时启用；停止时只影响选定实例：

```bash
sudo systemctl enable "$CP_SERVICE"
sudo systemctl stop "$CP_SERVICE"
```

模板使用 `Restart=no`，由管理员控制重启时机。`ProtectSystem`、`ProtectHome` 等选项限制网关服务自身，agent 的网络出口隔离由第 7 节的 `privacy-run` 建立。更多模板说明见 [部署目录](../deploy/README.md)。

## 7. 接入工具和可选运行模式

### 7.1 私密计算与 MCP

使用第 4 节的本机前台服务时，在可信客户端启动终端准备环境：

```bash
export CENSORPRIVACY_URL="http://127.0.0.1:$CP_PORT"
export CENSORPRIVACY_TOKEN="$(cat "$CP_CONTROL_DIR/gateway.token")"
export CENSORPRIVACY_RUN_ID="$(jq -er '.run_id' "$CP_CONTROL_DIR/run.json")"
CP_TOOL_DIR="$(dirname "$CP_BIN")"
```

systemd 部署时，由可信控制面读取该实例的 `/var/lib/censorprivacy/model-rust.run.json` 并向客户端提供本机 token 和 run ID。不要为了方便让 agent 读取整个 `/etc/censorprivacy`，其中还包含 provider key。

在现有客户端的 stdio MCP 配置中注册相应的绝对命令路径，并让其继承上述三个环境变量：

| 环境 | MCP command |
|---|---|
| 本机 release | `Rust源码目录/target/release/censorprivacy-mcp` |
| 系统部署 | `/usr/local/bin/censorprivacy-mcp` |

MCP 客户端完成 `initialize` 后可通过 `tools/list` 查询 `private_compute`。这是 stdio 进程，无需另开公网监听端口。CLI 示例为 `"$CP_TOOL_DIR/privcalc" sum "$REF_A" "$REF_B"`，其中两个变量必须是当前 run 已签发、未过期的真实引用，不能填原值或编造占位引用。计算结果继续受保护。

### 7.2 Linux 受管运行域

需要阻止 agent 绕过网关直接联网时使用 `privacy-run`。额外依赖：

```bash
# Ubuntu / Debian
sudo apt-get install -y iproute2 nftables util-linux libseccomp2
# openEuler / RHEL 系
sudo dnf install -y iproute nftables util-linux libseccomp
```

从 Rust release 或系统安装目录调用 `privacy-run`，控制面需要 root，目标进程必须使用专用非 root UID/GID。Rust 运行域需要支持 `mount_setattr`、`close_range`、pidfd、密封 memfd 的内核；验收环境为 Linux 6.6。容器还须获得必要 namespace、mount 和 nftables 能力。

先准备专属 workspace、只读运行依赖和私有控制文件，再按 [受管启动示例](../README.md#linux-受管启动) 设置参数。网关监听地址必须同时允许宿主 readiness 与运行域 veth 访问；本教程的 `127.0.0.1` 仅用于普通本机接入，需要配合监听地址及防火墙调整。

launcher 控制文件需由 root 或目标 UID 持有。systemd 网关的状态文件属于服务账户，管理员应为 launcher 提供当前 run 的 root 所有控制副本；不要扩大原凭据目录的读取权限。服务重启后重新提供当前状态。

### 7.3 TLS direct 的 native 库

此模式让协作的动态 OpenSSL 客户端保持原 TLS 连接。需要额外构建 C ABI hook：

```bash
# Ubuntu / Debian
sudo apt-get install -y libssl-dev
# openEuler / RHEL 系
sudo dnf install -y openssl-devel
```

接着在包含 `Cargo.toml` 和 `native/` 的仓库根目录执行。这里重新设置源码路径，便于在新终端中单独执行本节：

```bash
CP_RUST_SRC="$(pwd -P)"
make -C "${CP_RUST_SRC:?请先设置源码根目录}/native"
```

也可以在仓库根目录直接执行 `make -C native`。成功后生成 `native/libcp_tls_guard.so`；仅使用普通网关时无需构建此库。

产物为 `"$CP_RUST_SRC/native/libcp_tls_guard.so"`。可通过 `tls-run --native-library` 指定该可信绝对路径；也可以通过 `scripts/install.sh --with-native` 暂存该库，再以 root 所有、`0755` 权限安装到 `/usr/local/lib/censorprivacy/`；自定义前缀时也明确指定库路径。

网关增加 `--tls-direct`，上游使用实际 HTTPS IPv4 字面量 origin；客户端需要正确配置可信 CA 和目标身份验证。接入仅支持目标架构的动态 OpenSSL、HTTP/1.1、Content-Length、非流式未压缩 JSON，不支持 HTTP/2、SSE 或 chunked。`tls-run` 是协作式 hook，不建立独立强制运行域。完整接入参数和缓冲限制见 [native 说明](../native/README.md) 与 [TLS direct 示例](../README.md#tls-direct)。

## 8. 策略更新、版本更新与回退

策略修改后，将 `epoch` 增加到大于当前值，再触发重载：

```bash
sudoedit "/etc/censorprivacy/$CP_INSTANCE.policy.yaml"
sudo systemctl reload "$CP_SERVICE"
```

重载后再次检查认证 readiness。新 epoch 使旧引用失效；策略解析或重载失败时网关停用并拒绝继续处理。修正配置后由管理员恢复服务，不要把重载命令返回成功等同于策略已成功应用。

更新版本时，先在独立构建目录验证新产物并保存旧构建及配置，再停止目标服务、替换对应安装文件、启动并重复第 5 节验证。应保留旧归档和校验文件。回退也是停止后恢复旧产物和匹配配置，再启动验收。

每次重启都会创建新 run，旧 vault 无法从 `run.json` 恢复。通知客户端重新初始化会话与工具环境；不同服务实例之间也不能迁移旧引用。

## 9. 常见问题

| 现象 | 检查与处理 |
|---|---|
| make 报 `/native: No such file or directory` | 当前终端未设置 `CP_RUST_SRC`。回到仓库根目录执行 `CP_RUST_SRC="$(pwd -P)"`，或直接运行 `make -C native` |
| Cargo 提示锁文件需更新或 Rust 太旧 | 检查工具链、`Cargo.lock` 和 `rustup override`，依赖下载失败先解决包源连接 |
| 编译被杀或磁盘不足 | 检查可用内存与磁盘；内存较小时使用 `CARGO_BUILD_JOBS=1` 构建 |
| `/ready` 返回 401 | 客户端应使用该实例的 gateway token，检查 header 文件和端口是否对应 |
| readiness 成功但模型请求失败 | readiness 和 models 不访问上游。确认未使用 `.example` 占位地址，查看响应错误码，再检查实际 origin、凭据、模型 ID、CA 和路由；排除重复 `/v1` |
| Rust 启动返回固定错误码 | 检查凭据为私有普通文件、无叶子符号链接、run 目录 `0700`、监听端口未占用、策略有效 |
| systemd 报路径或权限错误 | 检查 `ExecStart` 绝对路径、文件所有权和 `ProtectHome` |
| 重载后服务拒绝处理 | 核对 epoch 已增加且策略有效；失败会停用，修正后重新启动并初始化客户端 |
| `privcalc` / MCP 失败 | 检查本机 URL、token、当前 run ID，以及引用的 run、epoch、TTL；不要传原值 |
| runtime 无法准备或无法访问网关 | 检查 root/namespace/nft 能力、内核、libseccomp、控制文件所有权及 veth 可访问的监听地址 |
| native 构建找不到头文件或 `-lcrypto` | 安装目标架构的 OpenSSL 开发包，检查 pkg-config 与链接路径 |
| TLS direct 拒绝请求 | 核对真实 IPv4 peer、客户端证书验证、动态 OpenSSL，以及 HTTP/1.1/长度/非流式限制 |

本教程命令依据当前 Rust CLI、安装脚本和 systemd 模板编写。已完成的 Rust 构建与测试证据见 [验收记录](validation.md)；新增教程本身不代表已在目标服务器安装或启动正式服务。
