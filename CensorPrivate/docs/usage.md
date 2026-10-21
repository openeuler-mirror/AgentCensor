# CensorPrivate 使用

AgentCensor 隐私子项目提供 cpctl 运维入口。

```bash
./cpctl doctor --tls --demo
./cpctl deps --tls --demo --dry-run
./cpctl install --tls
./cpctl demo gateway
./cpctl demo tls
```

安装系统依赖前先检查 dry-run；本机演示使用虚构策略和模拟模型，无需云凭据。
