# systemd deployment

首次部署可按 [构建部署教程](../docs/build-and-deploy.md) 操作。

自动部署入口见 [Rust 运维与使用手册](../docs/usage.md)：`./cpctl systemd install`、`./cpctl systemd start`、`./cpctl systemd check`。它从当前私有配置生成独立 unit，使用配置路径散列区分实例；不会覆盖此处手工维护的模板实例。

[censorprivacy@.service](censorprivacy@.service) 是完整网关的实例模板。每个实例对应独立 run、端口、token 和内存映射。路径可按安装布局修改；模板默认使用 `/usr/local/bin`、`/etc/censorprivacy` 和 `/var/lib/censorprivacy`。

## 准备

管理员先创建专用非 root 服务账户 `censorprivacy`，构建 Rust 二进制并通过 `scripts/install.sh --prefix /usr/local` 安装本项目，并准备服务账户可写的状态目录。`policy.yaml` 可从 [examples/policy.yaml](../examples/policy.yaml) 复制后配置。

以实例 `model` 为例，准备：

| 文件 | 内容 |
|---|---|
| `/etc/censorprivacy/model.env` | 公开的上游、监听地址、端口和模型 ID |
| `/etc/censorprivacy/model.policy.yaml` | 本地策略 |
| `/etc/censorprivacy/model.token` | 独立本机访问 token |
| `/etc/censorprivacy/model.upstream-key` | provider key |

环境文件仅包含公开配置：

```ini
CP_UPSTREAM=https://model.example
CP_BIND=127.0.0.1
CP_PORT=8765
CP_MODEL=MODEL_ID
```

token 和 upstream-key 为服务账户可读的私有普通文件，权限 `0600`；配置和凭据放在 agent workspace 之外，不允许 agent 修改。上游和模型 ID 应替换为实际配置。

## 安装与运行

```bash
sudo install -m 0644 deploy/censorprivacy@.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start censorprivacy@model
```

服务写入 `/var/lib/censorprivacy/model.run.json`，其中只有 run ID，不持久化映射。客户端使用该实例的网关地址和 token；私密计算需要额外注册 CLI 或 stdio MCP 工具。

模板提供 SIGHUP 重载，策略 `epoch` 必须增加：

```bash
sudo systemctl reload censorprivacy@model
sudo systemctl stop censorprivacy@model
```

实例模板不自动重启。重启会创建新 run 并丢失旧映射，客户端需重新初始化。

## 运行域与 TLS direct

模板的本机 HTTP 数据端口不应直接暴露到公共网络。配合 `privacy-run` 时，调整监听与防火墙，使本机 readiness 和受管 veth 均能访问；单独监听宿主 loopback 不足以服务运行域。launcher 的 token/run 状态控制文件必须由 root 或目标 UID 持有，服务账户不同时由管理员提供独立受控文件。

单实例 TLS direct 模板位于 [examples/censorprivacy.service](../examples/censorprivacy.service)，公开环境文件起点为 [examples/service.env.example](../examples/service.env.example)。它使用 Rust 二进制安装路径并启用 `--tls-direct`，需要实际的 HTTPS IPv4 origin、匹配架构的 native 库与客户端证书配置。重启会使旧客户端引用失效；模板默认关闭自动重启。

systemd 服务加固控制 broker 进程；受管 agent 的网络与文件边界由 `privacy-run` 单独建立。
