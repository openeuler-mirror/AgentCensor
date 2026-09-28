# CensorPivot 完整运行流程文档增量计划

## 目标

在已有组件审计、设计与 Demo 的基础上，补充一份可以独立阅读的完整运行流程文档，回答
Guard 策略何时真正生效、`VariantOpen` 创建了什么、`censorfs-mounter` 为什么存在，以及
三个组件如何绑定到同一棵工具进程树。

## 实施项

- [x] 复核 CensorGuard 策略加载、revision 持久化、双 bank 切换和 daemon 启动方式。
- [x] 复核 CensorFS `VariantOpen`、`AttachFuse` 和 `censorfs-mounter` 实现顺序。
- [x] 复核 Pivot runner Ready PID 与 CensorScope `track-add` 的衔接。
- [x] 新增 `CensorPivot/docs/flow.md`，分开描述部署准备与单批次运行。
- [x] 在 README 和设计文档中增加流程文档入口。
- [x] 执行 Markdown、链接和 diff 基础检查。

## 验收标准

1. 文档含不依赖 Mermaid 的完整纯文本流程图。
2. 明确说明 intent 预判是事前划界，不等于策略安装，也不替代 eBPF LSM 强制。
3. 明确说明 Guard 策略的来源、应用、生效、持久化与重启恢复路径。
4. 用通俗语言解释 `VariantOpen`、FUSE、Mount Namespace、`censorfs-mounter` 与 CAS 发布。
5. 给出一次工具写 `/workspace/result.txt` 时三个组件同时工作的具体路径。
