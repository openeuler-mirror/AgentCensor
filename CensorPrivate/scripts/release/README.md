# CensorPrivate @VERSION@ 二进制交付包

交付目标为 openEuler 24.03 LTS、x86_64、glibc ≥ @MIN_GLIBC@，需要 Bash、GNU coreutils、grep、sed 和 /proc。建议至少 2 GB 可用内存、2 GB 磁盘空间。其他版本须先核对 glibc；Windows、macOS 和 Alpine/musl 不能直接运行本包。

包内包含五个 Rust 命令、固定 bert4ner-base-chinese FP32 ONNX 模型、ONNX Runtime CPU、OpenSSL TLS hook、Node 和运维工具。不需要 Rust、Python、pip、npm、root 或下载模型。

## 快速验证

```bash
tar -xzf censorprivacy-@VERSION@-linux-x86_64-standalone.tar.gz
cd censorprivacy-@VERSION@-linux-x86_64-standalone
./cpctl self-test
```

预期最后输出“完整交付包自检”通过。该命令先校验 SHA-256，再启动真实本地检测器，以多段项目交付邮件分别验证 HTTP 网关和 OpenSSL TLS：人名、机构、手机号、邮箱、地址、日期、金额和秘密在上游成为占位符，普通隐私在本地恢复，SECRET 保持遮盖，私密求和恢复为 16000。上游是包内启动的本机模拟服务，不调用云模型；运行结束自动停止子进程。自检使用临时实例，不启动或续期日常使用的网关。

也可分别运行 `./cpctl verify`、`./cpctl doctor --tls --demo`、`./cpctl demo gateway`、`./cpctl demo tls`。首次启动会校验并加载约 389 MB 模型，请等待命令结束。

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

## 接入 Agent：DeepSeek Harness Web

先按 [DSH 官方说明](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.zh.md#run-from-source) 准备 Harness；源码方式需完成 `pnpm install` 和 `pnpm run build`。保持前面验证通过的网关运行，在本包目录执行（替换 Harness 源码目录）：

```bash
./cpctl dsh web --dsh-root /path/to/deepseek-harness
```

打开终端打印的带 `token` 的完整网址，默认地址为 `http://127.0.0.1:3080/`。首次进入点击“继续”，选择一个测试工作目录，确认模型为 `CensorPrivacy / deepseek-flash`。模型和凭据自动复用网关配置，无需再次填写 API Key。

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

Web 使用独立 profile，当前只开放 `private_compute` 工具。按 Ctrl+C 停止 Web，网关继续运行；网关重启后重新启动 Web 并新建会话。端口占用时加 `--port 3081`。PATH 已有可用的 `dsh` 命令时可省略 `--dsh-root`。

需要自动验证模型工具调用和私密求和时，另一个终端执行 `./cpctl dsh cloud --dsh-root /path/to/deepseek-harness`，预期客户 `DemoCustomerOrchid`、合计 `300.00`，记录保存在 `~/.config/censorprivacy/managed/dsh/cloud-report.json`。

## 接入 Agent：openEuler xiaoO

先准备 [xiaoO](https://atomgit.com/openeuler/xiaoO)。源码方式需要 Rust 1.88+，在其仓库执行 `cargo build --locked --release -p xiaoo-endside`；已有可用的 `xiaoo` 命令可直接使用。保持网关运行，在本包目录执行：

```bash
./cpctl xiaoo tui --xiaoo-root /root/xiaoO
```

在 xiaoO 终端界面粘贴上面的完整测试邮件，预期普通隐私恢复、SECRET 保持遮盖。模型地址、模型 ID 和网关凭据自动配置；xiaoO 的原配置保留，独立配置位于 `~/.config/censorprivacy/managed/xiaoo/`。首次消息需检测系统提示和完整工具目录，本机约等待 1–2 分钟，后续复用缓存。退出 TUI 后网关继续运行。PATH 已有 `xiaoo` 时可省略 `--xiaoo-root`；自定义构建位置使用 `--xiaoo-bin /绝对路径/xiaoo`，成功接入后会记住路径。

验证真实 Agent 工具调用，直接执行：

```bash
./cpctl xiaoo demo
```

该命令让 xiaoO 实际调用 `file_read` 读取虚构项目邮件，再由模型复述。工具返回内容出网时同样脱敏，最终显示应恢复两位联系人的信息及两笔费用，SECRET 不恢复。该命令使用实际模型并可能计费。

日常单次任务可用 `./cpctl xiaoo run '读取 privacy-business.txt，概括交付安排，不计算金额。'`，也支持从 stdin 读取文本。工作目录为独立配置下的 `workspace/`。本次接入支持网关模式及 xiaoO 原生 SSE、普通工具；普通工具参数中的隐私占位符不会还原成原值，DSH 的 `private_compute` 尚未注册到 xiaoO。xiaoO 自身的文件、Shell、网络工具权限由其运行环境管理。

其他 Agent：Base URL 使用 `http://127.0.0.1:8775/v1`，API Key 使用 `~/.config/censorprivacy/managed/gateway.token` 的内容，模型 ID 与 `init` 一致。

## 日常管理

```bash
./cpctl check
./cpctl stop
```

策略位于 `~/.config/censorprivacy/managed/policy.yaml`；修改业务词典/正则后停止并重新启动，客户端新建会话。映射只在内存中，重启后旧引用失效。已有配置会保留；独立实例在 init/start/check/stop 各命令中使用相同的 `--config-dir /绝对路径`，并在 init 指定不同 `--port`。

默认 `ttl_seconds: 3600` 表示本次 run 自启动起有效一小时，到期后进程仍在运行，但拒绝请求。出现 `vault_denied` 时执行 `./cpctl stop`、`./cpctl start`、`./cpctl check`，再重新打开 Agent 并新建会话。需要更长的会话时，可按需增大策略中的 `ttl_seconds`，重启生效。

完整包可在初始化前移动到其他目录；请保留 bin/、lib/、runtime/、share/ 的相对布局。不要设置指向其他环境的 CP_DETECTOR_MODEL_DIR、CP_DETECTOR_ORT_LIBRARY 或 ORT_DYLIB_PATH。五个底层命令在 bin/ 中，均支持 --help。TLS direct 支持范围是动态 OpenSSL、HTTP/1.1 JSON，演示会实际验证该链路。

Linux 出口隔离工具 privacy-run 另需 root、iproute2、nftables、libseccomp 和 namespace 能力；DSH 演示另需已安装的 DSH。这两项不属于本包的一键网关/TLS 自检。

版本、源码快照、依赖版本及系统 ABI 要求见 share/censorprivacy/BUNDLE.json；许可和来源声明见 share/censorprivacy/licenses/ 和 LICENSE。内部校验清单为 SHA256SUMS，外部归档校验文件为同名 .tar.gz.sha256。
