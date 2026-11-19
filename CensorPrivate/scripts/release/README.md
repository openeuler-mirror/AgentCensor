# CensorPrivate @VERSION@ 二进制交付包

交付目标为 openEuler 24.03 LTS、x86_64、glibc ≥ @MIN_GLIBC@，需要 Bash、GNU coreutils、grep、sed 和 /proc。建议至少 2 GB 可用内存、2 GB 磁盘空间。其他版本须先核对 glibc；Windows、macOS 和 Alpine/musl 不能直接运行本包。

包内包含五个 Rust 命令、固定 bert4ner-base-chinese FP32 ONNX 模型、ONNX Runtime CPU、OpenSSL TLS hook、Node 和运维工具。不需要 Rust、Python、pip、npm、root 或下载模型。

0.3.2 增加可配置 NER 线程数（`init --ner-threads N` 保存，`start --ner-threads N` 临时覆盖，默认 1）。`dsh cloud` 继承当前网关实际使用的 NER 配置；ONNX 不再给整篇文本共用 60 秒，每个窗口有 16 秒执行预算。DSH 默认只开放私密工具，`dsh run/web --enable-tools` 可开启原生工具，本机也可[注册 Python/Shell 私密工具](share/censorprivacy/docs/usage.md#注册自定义私密工具)。xiaoO 的工具仍默认关闭；显式开启时先在本机准备工具目录。

同一二进制支持 `start --ner-device cpu --ner-threads 4`、`start --ner-device cuda --ner-batch-size 4`，以及多卡 `--ner-device cuda:0,1,2,3`。`cuda` 默认第 0 张可见卡；窗口保持 512/64，batch 按每卡计算。包内仍提供 CPU 运行库；使用 CUDA 须另行准备匹配的 GPU ONNX Runtime、CUDA/cuDNN 和驱动，通过 `CP_DETECTOR_ORT_LIBRARY` 指定原生库。真实 GPU 运行须在 GPU 主机验收。昇腾使用 `--ner-device cann` 或 `cann:0,1`，需另配含 CANN provider 的 ORT、Ascend 驱动和 CANN Toolkit，同样须在对应硬件验收；本包为 x86_64，ARM 主机需要对应架构构建。

0.3.1 修复多轮对话中后续消息新增敏感值时返回 `detection_failed`：网关先识别整条请求的敏感值，再统一遮蔽全部历史并执行最终复核，Web 详情与实际出网文本保持一致。

同时修复 TLS 保护库路径含空格时的预加载问题，启动器通过保持打开的文件及依赖目录句柄加载，支持完整包迁移到含中文和空格的目录。

## 快速验证

```bash
tar -xzf censorprivacy-@VERSION@-linux-x86_64-standalone.tar.gz
cd censorprivacy-@VERSION@-linux-x86_64-standalone
./cpctl self-test
```

预期最后输出“完整交付包自检”通过。该命令先校验 SHA-256，再启动真实本地检测器，以多段项目交付邮件分别验证 HTTP 网关和 OpenSSL TLS：人名、机构、手机号、邮箱、地址、日期、金额和秘密在上游成为占位符，普通隐私在本地恢复，SECRET 保持遮盖，私密求和恢复为 16000。上游是包内启动的本机模拟服务，不调用云模型；运行结束自动停止子进程。自检使用临时实例，不启动或续期日常使用的网关。

也可分别运行 `./cpctl verify`、`./cpctl doctor --tls --demo`、`./cpctl demo gateway`、`./cpctl demo tls`。首次启动会校验并加载约 389 MB 模型，请等待命令结束。

## 打开本地隐私检测台

无需云账号，在解压目录执行：

```bash
./bin/censorprivacy web
```

浏览器打开 `http://127.0.0.1:8766/`，输入文本后检测；默认输入和会话记录为空，示例需主动点击载入。模型、分词器、运行库和网页均从包内加载，无需安装额外依赖或下载模型。Ctrl+C 停止。

## 接入实际模型

```bash
# 替换为自己的上游 origin 与模型 ID；交互输入 API Key，不回显。
./cpctl init --upstream https://api.deepseek.com --model-id deepseek-flash
./cpctl start
./cpctl check --model
```

上游地址不加 /v1。无需认证的本机模型可加 `--no-key`；HTTP 只用于受信任的本机测试。实际云请求需要联网及用户自己的有效凭据。

网关会为未指定 thinking 的 deepseek-* Chat Completions 请求补上 thinking: {"type":"disabled"}，兼容 xiaoO 等通用客户端。显式开启思考及非空 reasoning_content 仍会被拒绝；check --model 和 DSH 插件也会显式关闭思考。

连通性通过后，使用 `./cpctl demo gateway` 查看长文本的检测、上游占位符和本地恢复。

网关启动后，同一进程在 `http://127.0.0.1:8775/` 提供检测台，复用已加载的模型。观察 xiaoO 时使用此地址，无需再启动独立的 8766 检测台。

## 接入 Agent：DeepSeek Harness Web

先按 [DSH 官方说明](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.zh.md#run-from-source) 准备 Harness；源码方式需完成 `pnpm install` 和 `pnpm run build`。保持前面验证通过的网关运行，在本包目录执行（替换 Harness 源码目录）：

```bash
./cpctl dsh web --dsh-path /path/to/deepseek-harness
```

打开终端打印的带 `token` 的完整网址，默认地址为 `http://127.0.0.1:3080/`。首次进入点击“继续”，选择一个测试工作目录，确认模型为 `CensorPrivacy / deepseek-flash`。模型和凭据自动复用网关配置，无需再次填写 API Key。

gateway 模式使用真实 SSE，文字经网关校验和恢复后逐段显示。`./cpctl dsh run '任务'` 同样即时输出，终端不会再重复打印 headless 的最终总结；工具参数完整校验后才执行。TLS direct 继续使用完整 JSON 响应。NER 线程数在网关启动处配置：`./cpctl start --ner-threads 4`。

服务器通过 SSH 使用时，在自己电脑另开终端建立转发，再用本机浏览器打开上述完整网址：

```bash
ssh -N -L 3080:127.0.0.1:3080 root@服务器IP
```

将下面的完整文本复制到聊天框发送。它与 `demo gateway` 使用同一封虚构项目邮件：

```text
请逐字复述以下邮件，保留段落和完整占位符（包括尖括号），使用普通文本，不要使用代码块、表格或链接，不要计算金额。

主题：联合研究项目交付与付款安排（全部信息均为虚构测试数据）

各位同事：
张三在清华大学工作，负责本次联合研究项目的需求确认；李四在北京大学工作，负责验收资料整理。双方已完成需求评审，接下来先交付测试报告，再安排现场验收。请对外材料保留工作安排，但不要公开联系人和结算信息。

联系人与交付信息：
张三的电话是13800138000，邮箱是zhangsan@example.com；李四的电话是13900139000，邮箱是lisi@example.org。资料收货地址：北京市海淀区中关村东路1号。计划验收日期为2026年11月5日，具体会议时间由双方通过邮件确认。

付款说明：
首期服务费：12800.00元；后续支持费：3200.00元。
首期款在测试报告确认后支付，支持费在验收通过后支付。请财务分别核对这两笔费用，暂不要合并开票。若发票抬头或交付清单需要调整，请先联系张三，并抄送李四；回访仍使用13800138000，避免重复建档。

联调备注：
下面是虚构的临时测试凭据，仅供内部环境联调，不能放入对外纪要：secret=DEMO_SECRET_DO_NOT_SEND。
最终邮件需要保留交付顺序、付款条件和联系人对应关系。未确认的事项标注“待双方确认”，不要补写未提供的结论。
```

预期回复恢复姓名、机构、电话、邮箱、地址、日期和两笔金额，`DEMO_SECRET_DO_NOT_SEND` 不出现，通常显示为 `[REDACTED_SECRET]`。模型需要完整保留占位符才能恢复。上游脱敏内容可对照 `./cpctl demo gateway` 的输出查看。发送消息会调用实际模型并可能计费。

Web 使用独立 profile，当前只开放 `private_compute` 工具。按 Ctrl+C 停止 Web，网关继续运行；网关重启后重新启动 Web 并新建会话。端口占用时加 `--port 3081`。PATH 已有可用的 `dsh` 命令时可省略 `--dsh-path`。

需要自动验证模型工具调用和私密求和时，另一个终端执行 `./cpctl dsh cloud --dsh-path /path/to/deepseek-harness`，预期客户 `DemoCustomerOrchid`、合计 `300.00`，记录保存在 `~/.config/censorprivacy/managed/dsh/cloud-report.json`。

## 接入 Agent：openEuler xiaoO

先准备 [xiaoO](https://atomgit.com/openeuler/xiaoO)。源码方式需要 Rust 1.88+，在其仓库执行 `cargo build --locked --release -p xiaoo-endside`；已有可用的 `xiaoo` 命令可直接使用。网关启动时可用 `./cpctl start --ner-threads 4` 设置 NER 线程数；已有网关需先停止再启动。保持网关运行，在本包目录执行：

```bash
./cpctl xiaoo tui --xiaoO-path /root/xiaoO
```

另一个终端执行 `./cpctl xiaoo web` 查看观测地址，浏览器打开 `http://127.0.0.1:8775/`。页面自动同步经上述命令启动的 xiaoO 会话、流式回复和工具交互。点击消息下的“查看检测与遮蔽”，可对照用户输入、当次检测结果和实际发给模型的遮蔽文本；历史详情只读，刷新和网关重启后仍可查看。

开始新测试时，点击页面“清空记录”并确认，将删除本实例保存的全部 Web 会话及详情，其他打开的页面也会同步清空。清空不会重置 xiaoO 自身的上下文或网关内存映射；需要全新对话时，退出并重新运行 `./cpctl xiaoo tui`。记录位于私有配置目录的 `xiaoo/records/`，包含原始文本，请按本机私有数据保管。交付包不包含测试记录或用户凭据。

通过 SSH 访问时，在自己电脑执行 `ssh -N -L 8775:127.0.0.1:8775 root@服务器IP`，再打开本机的观测地址。

在 xiaoO 终端界面粘贴上面的完整测试邮件，预期普通隐私恢复、SECRET 保持遮盖。模型地址、模型 ID 和网关凭据自动配置；xiaoO 的原配置保留，独立配置位于 `~/.config/censorprivacy/managed/xiaoo/`。默认关闭工具；需要工具时加 `--enable-tools`，启动器会在进入界面前加载真实工具目录并预热检测，显示准备状态。该工作移至启动阶段，耗时仍取决于 CPU 与目录大小；准备请求不调用上游，也不计入 Web 会话。新的用户输入和 TUI 自身系统提示仍需正常检测。退出 TUI 后网关继续运行。`--xiaoO-path` 指定源码目录，依次查找 `target/release/xiaoo`、`target/debug/xiaoo`，成功接入后会记住路径；PATH 已有 `xiaoo` 时可省略。旧参数 `--xiaoo-root` 和 `--xiaoo-bin` 仍兼容。

验证真实 Agent 工具调用，直接执行：

```bash
./cpctl xiaoo demo --enable-tools
```

该命令让 xiaoO 实际调用 `file_read` 读取虚构项目邮件，再由模型复述。工具返回内容出网时同样脱敏，最终显示应恢复两位联系人的信息及两笔费用，SECRET 不恢复。该命令使用实际模型并可能计费。

日常单次任务可用 `./cpctl xiaoo run --enable-tools '读取 privacy-business.txt，概括交付安排，不计算金额。'`，也支持从 stdin 读取文本。工作目录为独立配置下的 `workspace/`。本次接入支持网关模式及 xiaoO 原生 SSE、普通工具；普通工具参数中的隐私占位符不会还原成原值，DSH 的 `private_compute` 尚未注册到 xiaoO。xiaoO 自身的文件、Shell、网络工具权限由其运行环境管理。

其他 Agent：Base URL 使用 `http://127.0.0.1:8775/v1`，API Key 使用 `~/.config/censorprivacy/managed/gateway.token` 的内容，模型 ID 与 `init` 一致。

## 日常管理

```bash
./cpctl check
./cpctl stop
```

策略位于 `~/.config/censorprivacy/managed/policy.yaml`；修改业务词典/正则后停止并重新启动，客户端新建会话。映射只在内存中，重启后旧引用失效。已有配置会保留；独立实例在 init/start/check/stop 各命令中使用相同的 `--config-dir /绝对路径`，并在 init 指定不同 `--port`。

默认 `ttl_seconds: 3600` 表示本次 run 自启动起有效一小时，到期后进程仍在运行，但拒绝请求。出现 `vault_denied` 时执行 `./cpctl stop`、`./cpctl start`、`./cpctl check`，再重新打开 Agent 并新建会话。需要更长的会话时，可按需增大策略中的 `ttl_seconds`，重启生效。

完整包通过可执行文件位置定位模型和运行库，请保留 bin/、lib/、runtime/、share/ 的相对布局。移动包或升级版本前，先用旧目录的 `./cpctl stop` 停止网关；在新目录运行 `./cpctl start`、`./cpctl check` 后重新启动 xiaoO，新建会话。既有配置、策略、凭据和 Web 记录会保留。不要设置指向其他环境的 CP_DETECTOR_MODEL_DIR、CP_DETECTOR_ORT_LIBRARY 或 ORT_DYLIB_PATH。五个底层命令在 bin/ 中，均支持 --help。TLS direct 支持范围是动态 OpenSSL、HTTP/1.1 JSON，演示会实际验证该链路。

Linux 出口隔离工具 privacy-run 另需 root、iproute2、nftables、libseccomp 和 namespace 能力；DSH 演示另需已安装的 DSH。这两项不属于本包的一键网关/TLS 自检。

版本、源码快照、依赖版本及系统 ABI 要求见 share/censorprivacy/BUNDLE.json；许可和来源声明见 share/censorprivacy/licenses/ 和 LICENSE。内部校验清单为 SHA256SUMS，外部归档校验文件为同名 .tar.gz.sha256。
