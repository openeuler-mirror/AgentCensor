# Rust 运维与演示验收记录

核心复核日期：2026-10-21；DSH 演示与 systemd 静态验证日期：2026-10-16。架构见 [架构说明](architecture.md)。操作方法见 [使用手册](usage.md)。

环境：openEuler / Linux 6.6 x86_64，Rust 1.85.1，Node 22.22.3，OpenSSL 3.0.12，动态 OpenSSL curl 8.4.0；本机已有 DeepSeek Harness 源码，版本 0.1.5-rc.2。

| 项目 | 实际结果 |
| --- | --- |
| 环境检查 | `cpctl doctor --tls --demo --dsh --dsh-root /root/deepseek-harness` 通过 |
| 系统依赖 | dnf 安装命令 dry-run 通过；未实际安装或升级系统包 |
| 安装部署 | release 构建、临时前缀安装、安装后从其他工作目录调用 cpctl 通过；解包产物的 doctor 不调用 Rust 编译器，源码布局仍检查最低版本 |
| 用户实例 | 初始化保留、0600 文件/0700 控制目录、start/check/stop、重启更换 run ID、模型探测通过 |
| 拒绝路径 | 重复启动、运行中更新、失效 PID 记录、非法 origin、错误文件权限、输出符号链接、无效参数拒绝通过 |
| 本机网关 | Rust 网关到模拟上游；上游无虚构原值、SECRET 遮盖、引用求和与最终 300.00 显示通过 |
| 本机 TLS | 真实 OpenSSL hook、Rust broker、自签 CA 校验；等长别名、计算和恢复、非法请求未转发通过 |
| DSH 网关 | 独立 headless profile、真实 agent、模拟模型两次往返、private_compute 和最终显示通过 |
| DSH TLS | 真实 agent 经 tls-run/curl 原 HTTPS 连接，工具别名转 CP 引用、派生结果恢复通过 |
| DSH 日常使用 | 安装产物的 `dsh setup/run` 通过；以 `--help --root` 开头的任务原样到达模拟上游，没有被两层 CLI 解析为选项 |
| 停止与 dry-run | 坏连接配置不阻止按身份停止；依赖预览显示实际包管理命令，systemd 预览的标准输出是独立 unit 正文 |
| 插件边界 | 不支持内容拒绝、计算结果额外原值字段拒绝、非法响应不部分输出、HTTP 总超时、取消等待进程结束通过 |
| 发布归档 | 含 cpctl、五个 Rust 命令、native 库、运维脚本、DSH 插件和独立文档，附 SHA-256 |
| 标准 npm bundle | 发布清单预览通过；未发布、未向已有 DSH profile 安装。实际演示通过绝对模块路径加载同一插件 |
| systemd 静态验证 | 网关与 TLS 两种实际生成 unit 均通过 `systemd-analyze verify`；本轮未安装账户/unit、未启动正式系统服务 |
| 云验收准备与失败路径 | TLS 复用已保存的自定义 native 库路径；故意失败的 DSH stub 验证配置传递、0600 失败报告及凭据不进入报告。此项未调用远端，不计为云效果验收 |
| 真实云服务 | 验收入口与报告已实现；缺有效实际上游/模型/凭据配置，本轮未执行，不计为通过 |
| 官方接入信息核对 | 已联网读取官方 quickstart 与 providers 页面，确认官方模型要求 API key；不带凭据的官方 `GET /models` 实际返回 401。此项仅核对接入要求，不是模型推理验收 |

可重复执行的本机验收：

```bash
bash scripts/test/operations.sh --dsh-root /root/deepseek-harness
```

其中 4 项 Node 测试覆盖插件边界和实际安装/生命周期，随后执行 DSH 的两种完整演示。安装生命周期测试还调用安装产物的网关和 TLS 演示；指定 DSH 路径时加入安装产物的 managed profile 准备及实际 agent 任务，因此同时验证源码和安装后的布局。监听、进程和临时目录仅用于回环测试，测试结束清理。

核心与扩展验收入口：

```bash
bash scripts/test/run-all.sh --native --runtime --operations
```

格式检查、Clippy `-D warnings`、Rust 核心测试（含真实 native、root 运行域及连续三次超时清理）和运维测试通过。207 组固定样例包括 80 组检测与 127 组精确计算，由 Rust 集成测试读取 `tests/fixtures/*.jsonl` 并比较结果，无需外部语言解释器；十进制比较保留精度与字段集合。

补充检查修复 TLS 云验收未沿用自定义 native 库路径的问题；`bash scripts/test/operations.sh` 的 4 项测试再次通过，包含安装后配置传递和云验收失败报告的回归。此回归使用保留测试地址和本机 DSH stub，在发起模型请求前失败，没有实际云调用。

systemd 静态验证分别使用无凭据的回环网关配置、有虚构凭据及测试 CA 的 TLS 配置，核对生成的认证、HTTP/TLS、CA 与库路径参数；没有向正式服务写入配置或启动实例。宿主已有其他 unit 的旧 `/var/run` 路径提示，不影响这两份 unit 验证退出成功。

真实云入口不是固定回复探测：它使用独立虚构策略要求模型发起引用求和，并检查工具回执与授权显示。没有有效配置、响应不受支持或模型没有正确调用工具时失败；网关与 TLS 各自遵守其协议限制。报告不能证明云服务内部的数据存储行为。
