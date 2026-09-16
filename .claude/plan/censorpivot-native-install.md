# CensorPivot 原生组件安装与使用计划

## 目标

提供从源码安装 CensorFS、CensorGuard、CensorScope 和 CensorPivot 的统一入口，
补齐服务用户、权限、配置与统一启动流程，并为缺失组件提供可执行的错误提示。

## 实施项

- [x] 新增 `fs`、`guard`、`scope`、`pivot`、`all` 安装子命令。
- [x] 新增统一 `start`、`stop`、`status`、`doctor` 运维子命令。
- [x] 安装 `censorpivot` 系统用户、sudoers、配置和 systemd 单元。
- [x] 统一组件路径、Guard 策略组和运行目录权限。
- [x] 改进 `censord` 缺失组件时的错误信息。
- [x] 新增 Pivot 从零安装使用文档并修正 README 入口。
- [x] 增加脚本测试并运行 shell、Rust、Git 校验。

## 验收标准

1. 可按 `fs -> guard -> scope -> pivot` 顺序安装，也可执行 `all`。
2. 安装器创建专用非 root `censorpivot` 用户，不再出现“无效的用户”错误。
3. `start` 只启动统一 `agentcensord` 和 `censorpivot`，不与组件独立服务冲突。
4. 缺少二进制、内核能力、配置或 socket 时显示组件名称和修复命令。
5. 文档覆盖首次安装、首次初始化、启动、检查、停止和常见错误。
