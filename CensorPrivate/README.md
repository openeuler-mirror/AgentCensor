# CensorPrivate

CensorPrivate 是 AgentCensor 的模型数据隐私子项目，提供本机策略检测、CP 引用、私密计算、HTTP/HTTPS 网关、SSE、MCP、Linux 运行域和 TLS direct。Rust 1.85+ 实现核心与服务；核心 crate 禁止 `unsafe`，Linux syscall/FFI 集中在独立 `runtime-sys` crate，C OpenSSL hook 负责动态 ABI 接入。产品命令与 Cargo 包名使用 `censorprivacy`。

架构及与 AgentCensor 其他组件的职责关系见 [架构说明](docs/architecture.md)，验证入口和实际结果见 [验收记录](docs/validation.md)。项目构建、测试与运行不依赖 Python。

从源码构建、本机验证到 systemd 部署，参阅 [构建部署教程](docs/build-and-deploy.md)。代码评审、分批提交与 PR 组织参阅 [贡献指南](CONTRIBUTING.md)。

希望用短命令操作时，从 [Rust 部署与使用手册](docs/usage.md) 开始：`./cpctl doctor` → `./cpctl deps` → `./cpctl install --tls` → `./cpctl demo gateway` / `./cpctl demo tls`。手册接着介绍策略与凭据、DSH 插件的两种演示和真实云验收。安装后也可使用 `~/.local/bin/cpctl`，无需保留源码目录或上一终端的变量。

CensorPrivacy 是本机模型隐私网关。它按本地策略检查完整请求，将敏感文本替换为当前 run 的引用，并在返回的被动显示文本中恢复授权内容。SECRET 原值不恢复；未知格式或保护失败时拒绝处理。

客户端通过已有 base URL 和工具配置接入，无需修改 harness 源码。一个服务实例维护一个 run，映射保存在服务内存中。

## 安装

需要 Rust 1.85 或以上版本及 C 链接器。常规网关使用 Rustls，无需 OpenSSL 开发库。

```bash
cargo build --locked --release --bins
# 安装到用户指定的暂存目录；正式安装可使用 /usr/local。
bash scripts/install.sh --prefix "$HOME/.local"
```

可用 `bash scripts/package.sh --with-native` 生成带 SHA-256 校验文件的 Linux 发布归档。systemd 模板使用 `/usr/local/bin`；自定义 prefix 时同步调整模板路径及 `tls-run --native-library`。

五个入口：`censorprivacy`、`privacy-run`、`privcalc`、`censorprivacy-mcp`、`tls-run`。也可通过 `target/release/` 直接运行。

```bash
bash scripts/test/run-all.sh
# 207 组固定检测/计算样例包含在常规 Rust 测试中
# 需要 Linux root、namespace/mount/nft 能力
bash scripts/test/runtime.sh
# 需要 OpenSSL 开发库，包含真实动态 ABI、分片和非阻塞重试
bash scripts/test/native.sh
```

可选的 TLS direct 模式需要 Linux、C11 编译器、make、OpenSSL 开发头文件及 libcrypto、pthread、libdl；pkg-config 可辅助查找编译参数。在目标架构上构建：

```bash
make -C native
```

生成的库为 `native/libcp_tls_guard.so`。安装 native 库时使用 `bash scripts/install.sh --with-native --prefix /usr/local`；直接运行时通过 `--native-library` 指定。

## 启动网关

先设置 `CONTROL_DIR` 为管理员管理的私有目录，在其中准备 `gateway.token` 和 `upstream.key`。token 使用 24–512 个可见 ASCII 字符；凭据文件权限为 `0600`，放在 agent workspace 和可见依赖树之外。两个文件分别用于本机认证和上游认证。

将上游 origin 和模型 ID 换成实际配置：

```bash
censorprivacy serve \
  --policy examples/policy.yaml \
  --upstream https://model.example \
  --model MODEL_ID \
  --token-file "$CONTROL_DIR/gateway.token" \
  --upstream-key-file "$CONTROL_DIR/upstream.key" \
  --run-state-file "$CONTROL_DIR/run.json" \
  --host 127.0.0.1 --port 8765
```

客户端使用网关 token，provider key 由服务持有。Chat Completions 和 Responses 客户端通常将 base URL 配置为 `http://127.0.0.1:8765/v1`；Messages 客户端应按其路径拼接方式配置服务 origin。

支持的模型入口为 `/v1/chat/completions`、`/v1/responses`、`/v1/messages` 和 `/v1/messages/count_tokens`。请求中的 system、历史、工具描述、JSON 参数、工具结果和 metadata 都经过检查。完整网关支持已实现的 JSON 与文本 SSE 响应格式。

本机 HTTPS 入口可增加 `--tls-cert` 和 `--tls-key`；自定义上游 CA 使用 `--upstream-ca-file`。上游保持证书和主机名校验。`privacy-run` 与 `tls-run` 的控制面探测目前使用本机 HTTP。

## 策略

[examples/policy.yaml](examples/policy.yaml) 是可复制的配置起点。支持 `epoch`、`dictionaries`、`patterns`、`mask_money`、`mask_dates`、`mask_all_numbers`、`default_labels` 和 `ttl_seconds`。填写业务词典与正则规则，或使用 `--sensitive-path` 指定整体保护的 JSON 字段路径；未知配置字段拒绝加载。

Rust 库通过 `Detector` / `LocalAnalyzer` 接入由调用方预先加载的本机检测器，`AnalyzerDetector` 映射标准实体类型并校验 Unicode 字符索引；网关通过 `PrivacyGate::with_detectors` 注册，重载时保留必需检测器。产品不自动下载模型。

规则未覆盖的名称、业务秘密和编码变形不会自动获得完整识别保证。业务数值需要计算时，应使用私密计算引用；不能通过把 JSON 数字改成字符串来保持协议兼容。

在支持 SIGHUP 的系统上，策略重载要求增加 `epoch`。旧引用随策略版本失效；服务重启会创建新 run 并丢失内存映射，客户端应重新初始化。

## Linux 受管启动

`privacy-run` 为启动的进程及后代创建受限运行域，出口只允许连接指定网关。需要 root 控制面、iproute2、nftables、unshare、libseccomp，以及支持 `mount_setattr`、`close_range`、pidfd 和密封 memfd 的内核。默认只读挂载公共 bin/lib/share 运行库目录，额外依赖通过 `--read-only-path` 明确添加。

```bash
sudo privacy-run \
  --run-state-file "$CONTROL_DIR/run.json" \
  --token-file "$CONTROL_DIR/gateway.token" \
  --workspace "$AGENT_WORKSPACE" \
  --uid "$AGENT_UID" --gid "$AGENT_GID" \
  --gateway-port 8765 \
  --read-only-path "$AGENT_RUNTIME" \
  --timeout 300 \
  -- "$AGENT_EXECUTABLE" AGENT_ARGS
```

这些变量应指向专属 workspace、非 root UID/GID、可信只读依赖和实际可执行文件。launcher 控制文件必须为私有普通文件，由 root 或目标 UID 持有。服务账户不同的部署应由管理员提供符合所有权要求的本次 token 和 run 状态文件。

网关需要同时可供本机 readiness 探测和运行域内的 veth 访问；仅监听宿主 `127.0.0.1` 不满足后者。按部署边界调整监听地址和访问规则。现有未受管进程不属于该运行域。

## TLS direct

`tls-run` 为协作的动态 OpenSSL 客户端加载 native 库，保持客户端原 TLS 连接，在加密前与交付响应前调用本机 broker。

服务增加 `--tls-direct`，上游配置为实际 HTTPS IPv4 字面量 origin。客户端示例：

```bash
tls-run \
  --broker http://127.0.0.1:8765 \
  --token-file "$CONTROL_DIR/gateway.token" \
  --run-state-file "$CONTROL_DIR/run.json" \
  --upstream-key-file "$CONTROL_DIR/upstream.key" \
  --native-library "$PWD/native/libcp_tls_guard.so" \
  --workspace "$AGENT_WORKSPACE" \
  --timeout 120 \
  -- "$CLIENT_EXECUTABLE" CLIENT_ARGS
```

此模式需要匹配目标证书的客户端配置，仅支持固定 IPv4 peer、HTTP/1.1、Content-Length 和非流式未压缩 JSON。SSE、HTTP/2、chunked、静态 TLS 和其他 TLS 后端不在支持范围内。`tls-run` 本身不创建强制网络隔离域。构建、接入与长度限制见 [native/README.md](native/README.md)。

## 私密计算

`privcalc` 和 `censorprivacy-mcp` 调用本机 `/v1/private_compute`。工具输入是已签发引用，服务内部解引用并执行受限 Decimal/日期运算；返回派生引用，保留敏感标签，不向模型返回原始操作数或结果。

```bash
privcalc aggregate "$REF_A" "$REF_B"
```

工具环境需要 `CENSORPRIVACY_URL`、`CENSORPRIVACY_TOKEN` 和 `CENSORPRIVACY_RUN_ID`，由 launcher 或可信控制面提供。CLI 支持 `sum`、`mean`、`min`、`max`、`aggregate`、`date_diff`；stdio MCP 入口为 `censorprivacy-mcp`。在客户端已有 MCP 配置中注册该命令，实际可用操作由 `tools/list` 返回。TLS direct 工具环境还需 `CENSORPRIVACY_MODE=tls-direct`。

普通 shell 或 Python 工具不会自动获得引用的解密权限。URL、代码、工具参数等主动使用位置不进行普通显示还原。

## 服务部署与范围

systemd 模板和配置步骤见 [deploy/README.md](deploy/README.md)。

完整网关保护配置到该入口的模型请求；独立出口限制需要 `privacy-run`。当前不支持任意媒体、opaque reasoning、远程状态、任意 TLS ABI 或通用私密代码执行。占位符会改变模型输入与 token 用量，使用场景应按实际策略评估。

## 许可证

沿用 AgentCensor 的 [木兰宽松许可证，第 2 版](LICENSE)。
