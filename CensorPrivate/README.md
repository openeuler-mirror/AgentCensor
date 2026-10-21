# CensorPrivate

AgentCensor 的模型数据隐私子项目。核心使用安全 Rust 实现，原值留在本机运行域，模型只接收引用。

采用 Rust 2021、最低 Rust 1.85，按策略、检测、vault、计算、网关与 TLS 逐层引入。

```bash
cargo check --locked --all-targets
```

`src/` 放核心实现，`crates/` 隔离必要 FFI，`tests/` 放回归，`native/` 放 OpenSSL ABI hook，`scripts/` 放运维入口，`docs/` 放教学文档。

本项目使用木兰宽松许可证第 2 版，见 [LICENSE](LICENSE)。
