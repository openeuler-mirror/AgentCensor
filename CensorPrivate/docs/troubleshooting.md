# 故障排查

[文档首页](README.md) · [构建与部署](build-and-deploy.md) · [使用手册](usage.md) · [脚本速查](scripts.md)

先确认当前实例与资源，再检查上游。`./censorprivacy doctor` 检查环境，`./censorprivacy check` 检查已启动实例，`./censorprivacy check --model` 才会调用实际模型。

## 1. 构建与检测资源

| 现象 | 原因与处理 |
| --- | --- |
| Cargo 编译成功，启动却返回 `detection_failed` | 编译不准备检测资源。检查 `CP_DETECTOR_MODEL_DIR` 和 `CP_DETECTOR_ORT_LIBRARY`；模型目录需有 manifest、ONNX、tokenizer 与 config，运行库必须是实际 `.so` 文件 |
| 提示未找到 ONNX Runtime | 配置 `CP_DETECTOR_ORT_LIBRARY=/绝对路径/libonnxruntime.so`；版本文件名也支持。源码模式不会自动搜索 Python 环境或下载运行库 |
| 环境变量已设仍加载旧路径 | YAML 优先于环境变量。检查 `ner.model_dir`、`ner.ort_library`；需要环境回退时将它们设为 `null`。`export` 仅对当前终端有效，长期部署建议在 YAML 中保存路径 |
| 模型文件齐全仍被拒绝 | 启动会验证哈希、模型 ID、FP32、标签、输入输出并进行真实预热。重新核对可信资源包与 `manifest.json`，不要手工改哈希绕过检查 |
| 从 Hugging Face 下载后不能启动 | 原始权重需要先 [导出 ONNX](build-and-deploy.md#4-准备检测资源)，不能直接把下载目录设为生产模型目录 |
| Cargo 提示工具链太旧或锁文件需更新 | 确认 Rust ≥1.85 和正确 checkout 的 `Cargo.lock`，用 `--locked`；先解决依赖源连接问题，不要为下载失败随意更新依赖 |
| 编译或模型进程被 `Killed` | 检查可用内存与磁盘。使用 `CARGO_BUILD_JOBS=1`、`RUST_TEST_THREADS=1`；编译、导出、推理与浏览器串行，确认旧进程退出再重试 |
| `doctor --demo` 提示 Node 不合格 | 需要 Node 22.19+（22.x）或 ≥24；DSH 0.2.2-rc.1 另需 Node ≥24.9。发行版包安装成功不代表版本满足。普通网关和检测台不依赖 Node |

资源变量示例：

```bash
export CP_DETECTOR_MODEL_DIR=/path/to/onnx/bert4ner-base-chinese
export CP_DETECTOR_ORT_LIBRARY=/path/to/libonnxruntime.so
./censorprivacy doctor
```

## 2. 认证与实例

| 现象 | 原因与处理 |
| --- | --- |
| `/ready` 返回 401 | 未认证探测的预期结果；受信任客户端必须使用同一实例的 `gateway.token`。不要拿 provider Key 连接本机网关 |
| `check` 提示认证或 run ID 不匹配 | 对齐端口、`--config-dir`、token 与 run 文件；系统实例先运行 `systemd check` 同步状态 |
| 进程存活但返回 `vault_denied` | 随附配置的 run TTL 为 86400 秒，省略时内置默认 3600 秒。执行 `stop → start → check`，重启 Agent 并新建会话；需要更长有效期时修改 `config.yaml` 的 `server.ttl_seconds` 后重启 |
| 修改后再次 `init` 没有效果 | `init` 保留已有配置。连接与凭据使用实例目录；检测、NER、TTL 等编辑 `config.yaml` 后重启，系统服务重新 `systemd install` |
| 提示权限错误 | 私有目录为当前用户所有且 0700，配置/凭据为 0600 普通文件，不使用叶子符号链接。配置目录置于 Agent 工作区之外 |
| 网关端口被占用 | 确认是否已有用户或系统实例。独立实例在 `init` 选不同 `--port`，后续各命令保持同一 `--config-dir` |
| 新参数被报 `unexpected argument` | 核对 `start` 打印的程序路径及 `service.log`。旧安装记录可能指向旧二进制；源码入口优先本仓库 `target/release/`，先串行执行 `CARGO_BUILD_JOBS=1 cargo build --locked --release --bins`，或使用更新后的完整包 |
| `stop` 报无匹配进程，但端口仍在监听 | 核对原启动目录与进程记录，勿按端口直接杀进程。新记录保存 PID、启动时刻和程序路径，支持跨入口停止同一实例；旧记录仍按原程序路径核验 |
| 重启后旧引用不能恢复 | 映射只在原进程内存中；新 run、策略 epoch 或 TTL 均会影响授权。新建 Agent 会话，不能只复制 `run.json` |

用户实例日志位于私有目录的 `service.log`，模型探测结果位于 `probe-response.json`。查看时保留其私有权限。

## 3. 上游与协议

| 现象 | 原因与处理 |
| --- | --- |
| `check` 通过但模型请求失败 | 本机就绪不代表上游可用。运行 `check --model`，核对 origin、实际模型 ID、Key、CA 和网络 |
| URL 变成重复 `/v1` 或路径被拒绝 | `init --upstream` 只填 origin，例如 `https://api.deepseek.com`；客户端 Base URL 通常使用本机 `/v1` |
| 模型返回 502 或 `protocol_blocked` | 检查受支持的请求/响应格式；未知内容、opaque reasoning、图片等不受支持格式会拒绝。原始上游错误正文不应成为公共日志 |
| 启动返回 `ner_device_unavailable` | 选择了 CUDA/CANN，但 ORT 不支持对应 provider、依赖缺失、设备编号不可见或初始化失败。将 `ner.ort_library` 指向对应的 ORT，并检查驱动、CUDA/cuDNN 或 CANN 及设备可见性。完整包默认附带 CPU 库；使用 CPU 时同时选择 CPU 库和 `ner.device: cpu` |
| 启动返回 `ner_config_invalid` | 检查 `CP_NER_DEVICE`、`CP_NER_BATCH_SIZE` 或嵌入配置；设备列表禁止重复，batch 为 1–256，默认 CPU/batch 1 |
| DeepSeek 思考回复被拒绝 | 使用关闭思考的 Chat Completions。网关仅对未指定 `thinking` 的 `deepseek-*` 请求自动补关闭值；显式开启仍拒绝，TLS direct 不自动改此字段 |
| SECRET 没有恢复 | 预期行为。SECRET 原值不恢复，显示为遮盖文本 |
| 普通隐私也没有恢复 | 模型需完整保留占位符，包括尖括号；只在授权的被动显示位置恢复。代码、URL 目标和普通工具参数不恢复 |

## 4. 页面与 Agent

| 现象 | 原因与处理 |
| --- | --- |
| 服务器页面无法从电脑浏览器打开 | 在自己电脑建立 SSH 转发，再打开本机地址。`censorprivacy start` 的检测台默认在 8775，使用 `ssh -N -L 8775:127.0.0.1:8775 用户@服务器IP`；单独运行 `censorprivacy web` 才默认使用 8766。DSH Web 默认转发 3080 |
| DSH Web 返回 401 / unauthorized | 打开 `censorprivacy dsh web` 启动日志中含 token 的完整网址，首次访问会建立浏览器会话。该 token 与网关的 `gateway.token` 不同；Web 重启后使用新日志中的网址 |
| 检测台提示忙 | 服务一次处理一个分析请求，等待当前请求结束再试；不要为重试同时启动多个模型进程 |
| 页面修改后没有变化 | 静态文件内嵌 Rust 二进制，重新编译或安装后重启服务，再刷新页面 |
| `dsh` 找不到或未构建 | 按官方文档完成构建，再传 `--dsh-path /绝对路径`；项目不会自动构建 Harness |
| DSH Web 模型或会话失效 | 核对当前网关并重启 Web，新建会话；命令行 profile 重新执行 `dsh setup` |
| xiaoO 启动不了 | 其源码构建需 Rust ≥1.88；使用 `--xiaoO-path` 指定包含已构建程序的源码目录，且当前配置须为 gateway 模式 |
| xiaoO 首次请求较慢 | 系统提示与完整工具目录需检测；等待当前请求，不要并发重启。工具和系统提示的重复分析会使用有界缓存 |
| 私密计算失败 | 输入必须为当前实例有效引用，检查 URL、token、run/epoch/TTL。xiaoO 当前尚未注册 DSH 的 `private_compute` |

## 5. systemd、TLS 与运行域

| 现象 | 原因与处理 |
| --- | --- |
| systemd 无法读取模型或运行库 | `ProtectHome=yes` 隐藏 home；改放 `/opt` 或公共安装目录，确认服务账户可读，并重新 `systemd install` |
| systemd 配置与用户实例不同步 | 停止系统实例，重新 `systemd install → start → check`。系统服务使用复制到 `/etc/censorprivacy/` 的配置 |
| native 找不到头文件或 `-lcrypto` | 安装目标架构 OpenSSL 开发包，检查 pkg-config/链接路径；执行 `make -C native` 时位于 `CensorPrivate/` 目录 |
| TLS direct 拒绝请求 | 核对动态 OpenSSL、真实 IPv4 peer、证书校验与 HTTP/1.1/Content-Length/非流式限制，见 [支持范围](../native/README.md#支持范围) |
| `censorprivacy run` 无法准备或访问网关 | 检查 root、namespace/mount/nft、内核与 libseccomp；网关需可从受管 veth 访问，宿主 loopback 单独监听不足以服务该运行域 |

需要核对脚本参数时先看 [脚本速查](scripts.md)，实际已执行过的测试范围以 [验收记录](validation.md) 为准。
