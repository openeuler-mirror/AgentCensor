# Demo Gallery

这些 demo 使用固定种子目录，目标是生成可复现的“赛马图 + Variant 活动时间线 + Validation + 对比表”证据。所有演示都在独立的本地 XFS/ext4 目录中初始化 CensorFS，不修改当前仓库。

## Scenarios

| Scenario | Seed | Validation | Expected evidence |
|---|---|---|---|
| `code-fix` | `demos/code-fix` | `python-code` | 重试同一订单只调用一次 gateway；至少一个 Candidate required validation 通过 |
| `static-site` | `demos/static-site` | `static-site` | HTML 可解析；三个视觉方案产生不同文件变更 |
| `harness-self-evolution` | `demos/harness-self-evolution` | `harness-config` | Candidate 保留有效 `cordis.patch.yml`；主 Agent 展示验证证据 |

## Run

先确保 `censorfs`、`censorfsd`、`censorfs-mounter` 在 `PATH` 中：

```bash
bash integrations/deepseek-harness/demos/run-demo.sh code-fix /var/tmp
```

脚本会自动初始化 seed 和 daemon。若设置 `DSH_DEMO_COMMAND`，还会启动指定的 Web Harness 命令；命令模板支持 `{PROMPT}` 和 `{ROOT}`，并通过 `DSH_DEMO_TIMEOUT_SECONDS` 控制等待时间：

```bash
DSH_DEMO_COMMAND='dsh --profile web --task "{PROMPT}" --session-events "$ROOT/evidence/session-events.json"' \
DSH_DEMO_TIMEOUT_SECONDS=1800 \
  bash integrations/deepseek-harness/demos/run-demo.sh code-fix /var/tmp
```

不同 Harness 版本的启动和事件导出参数可能不同，因此脚本不硬编码 CLI；未设置命令时会安全停在人工 `/explore` 入口。自动模式要求事件快照出现 `variant-prepared`，失败时保留 `harness.log` 和 daemon 日志。

## Capture

发布证据至少保留：

- Session 事件 JSON 快照；
- `harness.log`、daemon 日志和 commit id；
- `/branch-graph <run-id>` 输出或截图；
- 每个 Variant 的活动时间线、Validation 结果和最终 Compare 排名。

截图/GIF 应从全新 demo root 生成，避免混入旧 Candidate、旧 Session 或未提交工作树。完整发布门禁见 `docs/RELEASE_ACCEPTANCE.md`。
