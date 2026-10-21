# Native TLS transport

本目录提供 Linux x86_64/aarch64 的动态 OpenSSL 接入库。协作客户端通过 `LD_PRELOAD` 加载 `libcp_tls_guard.so`，原 TLS 连接由客户端管理；本机 broker 同步决定哪些请求和响应字节可以继续流动。

## 构建

需要 C11 编译器、make、OpenSSL 开发头文件、libcrypto、pthread 和 libdl。pkg-config 可用于查找头文件。针对运行机器的架构构建：

```bash
make -C native
```

输出为 `native/libcp_tls_guard.so`。通过 [tls-run](../README.md#tls-direct) 的 `--native-library` 指定它；库文件及其所在目录应由可信控制面管理，文件不能被组或其他用户写入。

## 接入

先使用 `censorprivacy serve --tls-direct` 启动 broker，配置固定的 HTTPS IPv4 origin，再由 `tls-run` 启动客户端。launcher 设置以下环境变量：

| 变量 | 用途 |
|---|---|
| `LD_PRELOAD` | native 库的绝对路径 |
| `CENSORPRIVACY_URL` | 本机 HTTP broker 地址；IPv4 loopback 或 `198.18.0.0/15` 运行域地址 |
| `CENSORPRIVACY_TOKEN` | 本次 broker 访问 token |
| `CENSORPRIVACY_TLS_PEER` | 批准的 IPv4 地址和端口 |
| `CENSORPRIVACY_RUN_ID` | 当前 run |

客户端必须完成握手并启用证书验证。库复核真实 TCP peer 与 OpenSSL 的验证结果；客户端仍负责正确配置可信 CA 和目标身份校验。缺少配置、peer 不匹配或 broker 不可用时拒绝数据调用。

## 支持范围

- 动态 OpenSSL 的受支持公开 API；不修改内部 SSL 对象布局或机器指令。
- HTTP/1.1 POST、Content-Length、完整未压缩 JSON、每连接单个在途请求。
- 单个 JSON 正文最多 256 KiB，头部和连接缓冲另有上限。
- 请求分片先留在本机，完整保护通过后才写入原连接；响应分片先交给 broker，完整安全响应形成后才返回应用。
- 短别名按敏感 span 的原始 JSON wire 字节预算分配，包括转义；不是按字符数或模型 token 数分配。1–2 字节 span 使用不可恢复遮盖。

HTTP/2、SSE、gzip、chunked、Upgrade、Expect、流水线、早期数据、sendfile 和 peek 等未支持入口拒绝处理。静态链接、其他 TLS 后端、自定义未暴露入口和通用非阻塞 readiness 兼容不在支持范围内。

本库是进程内协作接入，不提供对任意代码绕过的独立出口保证。需要受管进程的强制出口限制时，使用完整网关和 `privacy-run`。
