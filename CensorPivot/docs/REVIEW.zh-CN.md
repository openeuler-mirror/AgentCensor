# CensorPivot 代码与真实接口审查

## 结论与已修复问题

Pivot 的 Variant、Guard runner 注册和 Scope 调用都有真实实现，主流程不是纯演示。
本轮按设计、调用方参数、组件 CLI、服务端分发逐项核对，并修复以下缺口：

| 级别 | 原问题 | 修复 |
|---|---|---|
| P1 | 调用方可选择空组或复用域名，影响 domain 的策略绑定 | 管理员组白名单；domain 由事务 UUID 生成并持久化；注册前禁止执行用户工具 |
| P1 | root 启动可让 CensorFS View 和工具都归 root | `serve` 拒绝 root，runner 启动再次拒绝 UID/GID 0 |
| P1 | 未启用 mounter cgroup supervisor，进程组无法覆盖 setsid 后代 | 传入现有 cgroup 参数，等待 supervisor 清理成功才 Prepare |
| P1 | Ready、CLI、输出线程可无限等待；JSON 行可无限分配 | 共用安全 Rust 非阻塞进程封装，有界缓冲、轮询预算、超时及 Drop 清理 |
| P1 | `recover(uid)` 先推进所有人的事务，最后才过滤返回值 | 执行任何恢复动作前检查 owner UID，增加跨 UID 回归测试 |
| P2 | FS Variant 错误写 stdout，Pivot 却只读取 stderr | 保留 JSON `code/error`；Scope `message` 同样保留 |
| P2 | 日志临时文件名固定为 PID，崩溃残留可能阻碍后续保存 | 每次原子保存使用新 UUID 临时文件 |
| P2 | doctor 只检查路径存在，示例默认组与仓库策略命名不一致 | 增加 FS/Scope 往返、FUSE/cgroup 条件；示例统一为 `censorguard-dsh-default` |

实现继续禁止 `unsafe`。管道由 `ChildStdin/ChildStdout/ChildStderr` 拥有，通过 nix 的安全 FD
接口设置 nonblocking；不引入裸指针、手工 FD 所有权或异步运行时。原有两阶段决定不可翻转
语义保留，`evaluate_intent` 不接回执行链。

## Guard 0.5.0 适配

- 所有启动和诊断通过 `--config` 读取共用 YAML；删除旧 daemon flag 与 launcher 二进制依赖。
- runner 自身执行私有 v3 `register_self`，校验 domain/group 回执后才 Ready。
- Pivot doctor 实际查询 Guard readiness、配置路径、hooks 与管理员允许的已发布组。
- 命令适配器在调用前后和批次结束时核查 boot ID/runner 登记，变化时阻止继续执行与提交。
- 已有 JSON 可备份迁移到共用 YAML，未知选项在写入前拒绝。
- DSH 组合层更新 `typert-gateway`，保留原 inject 并增加工具/worker 启动门禁；打包前重建 Guard 前端。
- 基础策略与 daemon 设置在重启时读取；动态组独立恢复。运行中维护使用统一 stop/start。

## 组件接口核对

| 路径 | 实际接口依据 | 核对结果 |
|---|---|---|
| FS Open/Prepare/Publish/Abort | `CensorFs/cmd/censorfs/src/main.rs` 和 `crates/censorfs-core/src/control.rs` | 参数匹配，成功字段在 `result` 下；失败 stdout 为 `ok/code/error` |
| Mount/AttachFuse | `CensorFs/cmd/censorfs-mounter/src/main.rs` 和 `cgroup.rs` | 挂载、FD 传递、降权和 supervisor 均存在；本轮接入 supervisor |
| Guard 注册 | `CensorPivot/src/guard.rs`、Guard daemon `server.rs/runtime.rs` | v3 JSONL，`SO_PEERCRED` 取 PID；`set_scope_policy -> track_pid -> 成功响应校验 -> Ready -> Tool` |
| Scope 跟踪与 span | `CensorScope/crates/apps/ctl/src/args.rs/output.rs` 和 UDS transport | `--config/--socket-path/--json`、track、call 参数匹配；doctor 返回 `storage_ready/available_collectors` |

## 验证及复现

当前 master 已通过 Rust 全部测试、Clippy、安装器检查、真实 DSH CLI 的 Web/Headless
发布包组合检查，以及独立 BPF 的非 root runner 文件/exec 拒绝与审计验收。完整
CensorFS FUSE 事务和 Scope 采集 Demo 仍受本机 FUSE/cgroup 环境限制。

```bash
cd CensorPivot
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo test two_call_artifact_example_runs_with_real_tools
cargo test --test guard_launcher
cargo test --test guard_migration
bash scripts/test-install-agentcensor.sh
bash scripts/test-dsh-censorpivot.sh
```

普通测试包含真实 shell 执行示例的生成/校验两步，以及无限输出、持有管道的后台进程、
无 Ready/超长帧、结构化 FS 错误和恢复授权测试。注册测试使用真实
Pivot runner，注册端为临时 Unix socket fixture：拒绝、超时及错误回执时无 Ready，允许时 Ready PID 等于
`SO_PEERCRED` PID。fixture 不执行 `set_scope_policy/track_pid`，因此这不是内核强制验收。

本机缺少 `/dev/fuse` 和 cgroup v2 挂载，未执行完整 FUSE/Scope 采集与文件发布 Demo。
`python3 scripts/test-guard-runtime.py` 已验收真实 BPF、非 root runner 与审计；
此验收使用私有 mount namespace 下的临时 `/workspace`，不代表完成 CensorFS 验收。
FS/Scope 接口以源码和现有适配测试核对，不宣称已经运行了这两个真实 daemon。具备完整环境后使用 README 的 Demo 验证发布和 trace。

## 仍需部署验证的边界

- Pivot 与工具当前共享 CensorFS View owner UID；独立 Mount Namespace 不是完整沙箱。
  必须用 Guard 或外层隔离保护协调日志、控制 socket、配置、cgroup 状态等宿主资源，
  示例默认策略不代表已经覆盖这些路径。组白名单也不能替代这些保护。
- mounter supervisor 自身被 SIGKILL 时，cgroup 残留由下一次 mounter 启动回收；Pivot 的
  `recover` 本身不执行 root 级 cgroup 清理，也不自动补发崩溃期间丢失的 Scope CallEnd。
- Open 回复不确定时保持 AbortDecided 并重放 Open；持续 HeadChanged 等错误需要运维处理，
  当前没有组件级查询“该 request 是否已创建资源”的独立恢复接口。
- Guard 拒绝一次 syscall 不保证工具最终返回非零。Pivot 根据工具结果决定 Prepare；
  工具吞掉 EPERM 后仍返回 0 时，不应宣称“任意 Guard DENY 必定使整批 Abort”。
- eBPF hook 健康、真实挂载、父进程死亡清理和 Scope 事件归因仍需在完整环境验收。
