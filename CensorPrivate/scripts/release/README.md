# CensorPrivacy @VERSION@ 二进制交付包

交付目标为 openEuler 24.03 LTS、x86_64、glibc ≥ @MIN_GLIBC@，需要 Bash、GNU coreutils、grep、sed 和 /proc。建议至少 2 GB 可用内存、2 GB 磁盘空间。其他版本须先核对 glibc；Windows、macOS 和 Alpine/musl 不能直接运行本包。

包内包含统一 Rust 命令 `censorprivacy`、固定 bert4ner-base-chinese FP32 ONNX 模型、ONNX Runtime CPU、OpenSSL TLS hook、Node 和运维工具。不需要 Rust、Python、pip、npm、root 或下载模型。

部署参数集中在 `share/censorprivacy/config.yaml`，包括类别、自定义规则、TTL、NER 线程/设备/batch 和超时，详见 [统一配置](share/censorprivacy/docs/configuration.md)。默认全部类别开启；省略 `categories`、`null` 或空列表关闭按类别检测。每个 512 token 窗口默认有 16 秒执行预算，CP 引用后缀为 8 位小写字母和数字。

DSH 默认只提供私密工具，`--enable-tools` 可开启 Harness 工具；也可[注册 Python/Shell 私密工具](share/censorprivacy/docs/usage.md#注册自定义私密工具)。`dsh cloud` 继承当前网关的 NER 设置。xiaoO 默认关闭自身工具，显式开启时先在本机准备工具目录。

包内运行库使用 CPU。CUDA/CANN 需要另配对应的 ORT、驱动和设备依赖，并修改 `ner.ort_library`、`ner.device`；详见 [检测器配置](share/censorprivacy/docs/rust-detector.md#2-配置路径)。加速硬件须单独验收；ARM 主机需要对应架构的构建。

## 快速验证

```bash
tar -xzf censorprivacy-@VERSION@-linux-x86_64-standalone.tar.gz
cd censorprivacy-@VERSION@-linux-x86_64-standalone
./censorprivacy self-test
```

预期最后输出“完整交付包自检”通过。该命令先校验 SHA-256，再启动真实本地检测器，以虚构费用工单分别验证 HTTP 网关和 OpenSSL TLS：人名、机构、手机号、邮箱、地址、日期、金额和秘密在上游成为占位符，普通隐私在本地恢复，SECRET 保持遮盖，私密求和恢复为 2100.00。上游是包内启动的本机模拟服务，不调用云模型；运行结束自动停止子进程。自检使用临时实例，不启动或续期日常使用的网关。

只校验文件可运行 `./censorprivacy verify`。首次启动会加载约 389 MB 模型，请等待命令结束。

## 打开本地隐私检测台

无需云账号，在解压目录执行：

```bash
./bin/censorprivacy web
```

浏览器打开 `http://127.0.0.1:8766/`，输入文本后检测；默认输入和会话记录为空，示例需主动点击载入。模型、分词器、运行库和网页均从包内加载，无需安装额外依赖或下载模型。Ctrl+C 停止。

## 接入实际模型

```bash
# 替换为自己的上游 origin 与模型 ID；交互输入 API Key，不回显。
./censorprivacy init --upstream https://api.deepseek.com --model-id deepseek-flash
./censorprivacy start
./censorprivacy check --model
```

上游地址不加 /v1。无需认证的本机模型可加 `--no-key`；HTTP 只用于受信任的本机测试。实际云请求需要联网及用户自己的有效凭据。

网关会为未指定 thinking 的 deepseek-* Chat Completions 请求补上 thinking: {"type":"disabled"}，兼容 xiaoO 等通用客户端。显式开启思考及非空 reasoning_content 仍会被拒绝；check --model 和 DSH 插件也会显式关闭思考。

网关启动后，同一进程在 `http://127.0.0.1:8775/` 提供检测台，复用已加载的模型。观察 xiaoO 时使用此地址，无需再启动独立的 8766 检测台。

## 接入 Agent：DeepSeek Harness Web

对接基线为 **DSH 0.2.2-rc.1**（官方标签 `dsh-v0.2.2-rc.1`），需要 Node ≥24.9；完整包内置 Node 24 运行时。DSH 另行安装，不包含在本包中。安装时使用满足版本要求的 Node：

```bash
npm install --prefix /path/to/dsh-runtime @deepseek-ai/dsh@0.2.2-rc.1
```

保持前面验证通过的网关运行，在本包目录执行：

```bash
./censorprivacy dsh web --dsh-path /path/to/dsh-runtime
```

`--dsh-path` 也接受 npm 包目录、`.bin/dsh` 或按 [该版官方说明](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.1/README.zh.md#run-from-source) 构建的源码目录。

打开终端打印的带 `token` 的完整网址，默认地址为 `http://127.0.0.1:3080/`。首次进入点击“继续”，选择一个测试工作目录，确认模型为 `CensorPrivacy / deepseek-flash`。模型和凭据自动复用网关配置，无需再次填写 API Key。

gateway 模式使用真实 SSE，文字经网关校验和恢复后逐段显示。`./censorprivacy dsh run '任务'` 同样即时输出，终端不会再重复打印 headless 的最终总结；工具参数完整校验后才执行。TLS direct 继续使用完整 JSON 响应。

服务器通过 SSH 使用时，在自己电脑另开终端建立转发，再用本机浏览器打开上述完整网址：

```bash
ssh -N -L 3080:127.0.0.1:3080 root@服务器IP
```

将包内 `share/censorprivacy/examples/demo.txt` 的完整文本复制到聊天框发送。它与 `demo gateway` 使用同一份虚构费用工单，包含复述要求和测试数据。

预期回复恢复姓名、机构、电话、邮箱、地址、日期和两笔金额，`DEMO_SECRET_DO_NOT_SEND` 不出现，通常显示为 `[REDACTED_SECRET]`。模型需要完整保留占位符才能恢复。上游脱敏内容可对照 `./censorprivacy demo gateway` 的输出查看。发送消息会调用实际模型并可能计费。

Web 使用独立 profile，默认提供 `private_compute` 和已注册的私密工具；加 `--enable-tools` 可开放 Harness 工具。按 Ctrl+C 停止 Web，网关继续运行；网关重启后重新启动 Web 并新建会话。端口占用时加 `--port 3081`。PATH 已有可用的 `dsh` 命令时可省略 `--dsh-path`。

需要自动验证模型工具调用和私密求和时，另一个终端执行 `./censorprivacy dsh cloud --dsh-path /path/to/deepseek-harness`，预期客户 `DemoCustomerOrchid`、合计 `300.00`，记录保存在 `~/.config/censorprivacy/managed/dsh/cloud-report.json`。

## 接入 Agent：openEuler xiaoO

先准备 [xiaoO](https://atomgit.com/openeuler/xiaoO)。源码方式需要 Rust 1.88+，在其仓库执行 `cargo build --locked --release -p xiaoo-endside`；已有可用的 `xiaoo` 命令可直接使用。保持网关运行，在本包目录执行：

```bash
./censorprivacy xiaoo tui --xiaoO-path /path/to/xiaoO
```

另一个终端执行 `./censorprivacy xiaoo web` 查看观测地址，浏览器打开 `http://127.0.0.1:8775/`。页面自动同步经上述命令启动的 xiaoO 会话、流式回复和工具交互。点击消息下的“查看检测与遮蔽”，可对照用户输入、当次检测结果和实际发给模型的遮蔽文本；历史详情只读，刷新和网关重启后仍可查看。

开始新测试时，点击页面“清空记录”并确认，将删除本实例保存的全部 Web 会话及详情，其他打开的页面也会同步清空。清空不会重置 xiaoO 自身的上下文或网关内存映射；需要全新对话时，退出并重新运行 `./censorprivacy xiaoo tui`。记录位于私有配置目录的 `xiaoo/records/`，包含原始文本，请按本机私有数据保管。交付包不包含测试记录或用户凭据。

通过 SSH 访问时，在自己电脑执行 `ssh -N -L 8775:127.0.0.1:8775 root@服务器IP`，再打开本机的观测地址。

在 xiaoO 终端界面粘贴上面的完整测试邮件，预期普通隐私恢复、SECRET 保持遮盖。模型地址、模型 ID 和网关凭据自动配置；xiaoO 的原配置保留，独立配置位于 `~/.config/censorprivacy/managed/xiaoo/`。默认关闭工具；需要工具时加 `--enable-tools`，启动器会在进入界面前加载真实工具目录并预热检测，显示准备状态。该工作移至启动阶段，耗时仍取决于 CPU 与目录大小；准备请求不调用上游，也不计入 Web 会话。新的用户输入和 TUI 自身系统提示仍需正常检测。退出 TUI 后网关继续运行。`--xiaoO-path` 指定源码目录，依次查找 `target/release/xiaoo`、`target/debug/xiaoo`，成功接入后会记住路径；PATH 已有 `xiaoo` 时可省略。

验证真实 Agent 工具调用，直接执行：

```bash
./censorprivacy xiaoo demo --enable-tools
```

该命令将包内 `examples/demo.txt` 复制到独立工作区的 `demo.txt`，让 xiaoO 实际调用 `file_read` 读取虚构费用工单，再由模型复述。工具返回内容出网时同样脱敏，最终显示应恢复两位联系人的信息及两笔费用，SECRET 不恢复。该命令使用实际模型并可能计费。

日常单次任务可用 `./censorprivacy xiaoo run '请整理以下交接信息：……'`，也支持从 stdin 读取文本。工作目录为独立配置下的 `workspace/`；普通 `setup/tui/run` 不读取或复制演示文件，DSH 的 `setup/run/web` 也不依赖该文件。本次接入支持网关模式及 xiaoO 原生 SSE、普通工具；普通工具参数中的隐私占位符不会还原成原值，DSH 的 `private_compute` 尚未注册到 xiaoO。xiaoO 自身的文件、Shell、网络工具权限由其运行环境管理。

其他 Agent：Base URL 使用 `http://127.0.0.1:8775/v1`，API Key 使用 `~/.config/censorprivacy/managed/gateway.token` 的内容，模型 ID 与 `init` 一致。

## 日常管理

```bash
./censorprivacy check
./censorprivacy stop
```

统一配置位于 `share/censorprivacy/config.yaml`，业务词典和正则直接写在 `custom` 中，有效期通过 `server.ttl_seconds` 设置；修改业务词典/正则后停止并重新启动，客户端新建会话。映射只在内存中，重启后旧引用失效。已有配置会保留；独立实例在 init/start/check/stop 各命令中使用相同的 `--config-dir /绝对路径`，并在 init 指定不同 `--port`。

`server.ttl_seconds` 从启动起计时，随附配置为 86400 秒（24 小时），省略时默认 3600 秒（1 小时）。到期后进程仍在运行，但拒绝请求。出现 `vault_denied` 时执行 `./censorprivacy stop`、`./censorprivacy start`、`./censorprivacy check`，再重新打开 Agent 并新建会话。需要更长的会话时，可调大 `server.ttl_seconds` 并重启。

完整包通过可执行文件位置定位模型和运行库，请保留 bin/、lib/、runtime/、share/ 的相对布局。移动包或升级版本前，先用旧目录的 `./censorprivacy stop` 停止网关；在新目录运行 `./censorprivacy start`、`./censorprivacy check` 后重新启动 xiaoO，新建会话。既有配置、策略、凭据和 Web 记录会保留。不要设置指向其他环境的 CP_DETECTOR_MODEL_DIR、CP_DETECTOR_ORT_LIBRARY 或 ORT_DYLIB_PATH。底层可执行文件为 bin/censorprivacy，各功能子命令均支持 --help。TLS direct 支持范围是动态 OpenSSL、HTTP/1.1 JSON，演示会实际验证该链路。

Linux 出口隔离工具 censorprivacy run 另需 root、iproute2、nftables、libseccomp 和 namespace 能力；DSH 演示另需已安装的 DSH。这两项不属于本包的一键网关/TLS 自检。

版本、源码快照、依赖版本及系统 ABI 要求见 share/censorprivacy/BUNDLE.json；许可和来源声明见 share/censorprivacy/licenses/ 和 LICENSE。内部校验清单为 SHA256SUMS，外部归档校验文件为同名 .tar.gz.sha256。
