# CensorPivot 移除 Guard intent 预判增量计划

## 决策

Pivot 不再调用 CensorGuard `evaluate_intent`。安全策略只在受控执行链中生效：
`censorguard-exec` 调用 `register_self`，daemon 将 domain/group 和真实 PID 写入内核 map，
成功后 launcher 才 `exec` batch runner。实际 file/exec/network 行为始终由 eBPF LSM 强制。

## 实施项

- [x] 删除 Engine 的 Guard 预判阶段、trait 和泛型依赖。
- [x] 删除 Pivot 请求中的 intent 字段及相关模型。
- [x] 删除 Guard control socket 配置与 doctor 依赖，只保留 launch socket 和 launcher。
- [x] 更新 Demo、示例请求和测试。
- [x] 更新 README、设计、组件和完整流程文档。
- [x] 执行 fmt、test、clippy、文档链接和 diff 检查。

## 验收标准

1. Pivot 不再连接 Guard `ctl.sock`，也不发送 `evaluate_intent`。
2. 请求协议不再要求或接受工具 intent。
3. `guard_group` 仍传给 `censorguard-exec --group`。
4. 文档明确标出 `register_self` 内核映射成功是 domain 生效点，且发生在 `exec runner` 前。
5. CensorGuard 组件文档仍如实记录 `evaluate_intent` 是 Guard 已有能力，但注明 Pivot 不使用。
