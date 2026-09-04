// AttachSelf 客户端 + 心跳守护
// 连 /run/censorguard/dsh.sock, 调 attach_self, 周期校验 daemon boot ID 一致性。
// 传输层是 v2 NDJSON 信封 (request_id 配对), 与 daemon-unix 客户端同构。

import { randomUUID } from 'node:crypto';
import net from 'node:net';
import {
  AttachError,
  MAX_LINE_BYTES,
  PROTO_VERSION,
  Sockets,
  type RpcMethod,
  type RpcParams,
  type RpcRequest,
  type RpcResponse,
} from './proto.js';
import { ProtectionStateMachine } from './state.js';

// AttachSelf 调用参数
export interface AttachOptions {
  dshSockPath?: string; // 默认 /run/censorguard/dsh.sock
  policyGroup: string; // 必须在 daemon --dsh-self-groups 白名单内 (默认 censorguard-dsh-default)
  instanceHint?: string; // 可选, 多实例 DSH 区分用 (域名后缀)
  seed?: boolean; // 是否递归回填子孙进程 (默认 true)
  // 心跳参数: 周期校验 daemon boot ID, 变化触发重 attach
  heartbeatIntervalMs?: number; // 默认 5000
  // 校验期望值 (attach 回执与期望不符时报 version_mismatch)
  expectDaemonBootId?: string; // daemon 重启后会变, 留空表示不校验
  // 单次 RPC 超时 (默认 5000)
  timeoutMs?: number;
}

// AttachResult attach_self 成功后的回执
export interface AttachResult {
  domainId: number;
  domain: string;
  group: string;
  version: number;
  daemonBootId: string;
  hooksHealthy: boolean;
}

// AttachSelfClient Bootstrap 持有的客户端, 持有 socket 路径和心跳定时器。
// dispose 时只断开连接, 不调 untrack: 域生命周期由 daemon 按根进程退出回收。
export class AttachSelfClient {
  private readonly sockPath: string;
  private readonly timeoutMs: number;
  private heartbeatTimer: NodeJS.Timeout | null = null;
  private disposed = false;

  constructor(private opts: Required<Pick<AttachOptions, 'policyGroup'>> & AttachOptions) {
    this.sockPath = opts.dshSockPath ?? Sockets.dsh;
    this.timeoutMs = opts.timeoutMs ?? 5000;
  }

  /**
   * 执行一次 attach_self: 连 dsh.sock → 发请求 → 校验回执 → 返回 AttachResult。
   * 失败抛 AttachError, 调用方决定是否重试 / 转 degraded。
   */
  async attachSelf(): Promise<AttachResult> {
    if (this.disposed) throw new AttachError('connect', 'client 已 dispose');

    let resp: RpcResponse;
    try {
      resp = await this.call('attach_self', {
        policy_group: this.opts.policyGroup,
        instance_hint: this.opts.instanceHint,
        seed: this.opts.seed ?? true,
        // 显式不传 pid: daemon 用 SO_PEERCRED 取真实 pid, 客户端无法伪造
      });
    } catch (e) {
      if (e instanceof AttachError) throw e;
      throw new AttachError('connect', `连 dsh.sock 失败 (daemon 未运行?): ${(e as Error).message}`);
    }

    const body = unwrap(resp);
    const groupDenied = body.errorCode === 'permission_denied';
    if (!body.ok) {
      throw new AttachError(
        groupDenied ? 'group_not_allowed' : 'attach_failed',
        body.error ?? 'attach_self 未知失败',
      );
    }

    // 校验必需 hook 是否全部已挂载
    if (body.hooksHealthy === false) {
      throw new AttachError('hooks_unhealthy', '必需 hook 未全部挂载, daemon 可能降级');
    }

    // daemon boot ID 校验: 第一次 attach 不校验, 重 attach 时与上次比对
    if (this.opts.expectDaemonBootId && body.daemonBootId !== this.opts.expectDaemonBootId) {
      throw new AttachError(
        'version_mismatch',
        `daemon boot ID 变化 (期望 ${this.opts.expectDaemonBootId}, 实际 ${body.daemonBootId}), daemon 已重启`,
      );
    }

    const d = body.domain;
    if (!d || !d.name || !d.group) {
      throw new AttachError('attach_failed', 'attach_self 回执缺 Domain 信息');
    }

    return {
      domainId: d.id,
      domain: d.name,
      group: d.group,
      version: d.version,
      daemonBootId: body.daemonBootId ?? '',
      hooksHealthy: body.hooksHealthy ?? false,
    };
  }

  /**
   * 启动心跳守护: 周期 status_self 校验 daemon boot ID 未变化, 变化则触发重 attach。
   * 失败时通过状态机切 degraded 并回调 onReattachNeeded。
   */
  startHeartbeat(stateMachine: ProtectionStateMachine, onReattachNeeded: () => void): void {
    const interval = this.opts.heartbeatIntervalMs ?? 5000;
    if (this.heartbeatTimer) clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = setInterval(() => {
      void (async () => {
        if (this.disposed) return;
        try {
          const cur = stateMachine.get();
          if (cur.kind !== 'protected') return; // degraded/attaching 态不查
          const resp = await this.call('status_self', {});
          const body = unwrap(resp);
          if (!body.ok || !body.daemonBootId) {
            stateMachine.toDegraded('status_self 失败: ' + (body.error ?? '无 boot_id'));
            onReattachNeeded();
            return;
          }
          if (body.daemonBootId !== cur.daemonBootId) {
            // daemon 重启了, 当前域已失效, 触发重 attach
            stateMachine.toAttaching();
            onReattachNeeded();
          }
        } catch {
          stateMachine.toDegraded('心跳查询失败');
          onReattachNeeded();
        }
      })();
    }, interval);
  }

  /** 只断开连接和定时器, 不调 untrack (域由 daemon 按根进程退出回收) */
  dispose(): void {
    this.disposed = true;
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  // 每连接一条请求一条响应即关 (daemon 每连接只应答一行)
  private call(method: RpcMethod, params: RpcParams): Promise<RpcResponse> {
    return new Promise((resolve, reject) => {
      const request: RpcRequest = {
        v: PROTO_VERSION,
        request_id: `dsh-${process.pid}-${randomUUID()}`,
        method,
        params,
      };
      const conn = net.createConnection({ path: this.sockPath });
      const timer = setTimeout(() => {
        conn.destroy();
        reject(new AttachError('connect', `dsh.sock 连接超时 (${this.timeoutMs}ms)`));
      }, this.timeoutMs);

      conn.on('error', (err) => {
        clearTimeout(timer);
        conn.destroy();
        reject(new AttachError('connect', `dsh.sock 连接失败: ${err.message}`));
      });

      conn.on('connect', () => {
        conn.write(JSON.stringify(request) + '\n');
      });

      let buf = '';
      conn.on('data', (data) => {
        buf += data.toString('utf8');
        if (Buffer.byteLength(buf, 'utf8') > MAX_LINE_BYTES) {
          clearTimeout(timer);
          conn.destroy();
          reject(new AttachError('connect', `dsh.sock 响应超过 ${MAX_LINE_BYTES} 字节`));
          return;
        }
        const nl = buf.indexOf('\n');
        if (nl < 0) return;
        const line = buf.slice(0, nl);
        clearTimeout(timer);
        try {
          const resp = JSON.parse(line) as RpcResponse;
          if (resp.request_id !== request.request_id) {
            reject(new AttachError('connect', 'dsh.sock 响应 request_id 不匹配'));
            return;
          }
          resolve(resp);
        } catch (e) {
          reject(new AttachError('connect', `dsh.sock 响应解析失败: ${(e as Error).message}`));
        } finally {
          conn.end();
        }
      });
    });
  }
}

// 拆 v2 信封: ok=false 时提取 error code/message; ok=true 时摊平 result.response
interface UnwrappedBody {
  ok: boolean;
  error?: string;
  errorCode?: string;
  domain?: {
    name: string;
    id: number;
    slot: number;
    group: string;
    roots: number;
    version: number;
    draining?: boolean;
  };
  daemonBootId?: string;
  hooksHealthy?: boolean;
}

function unwrap(resp: RpcResponse): UnwrappedBody {
  if (!resp.ok) {
    return {
      ok: false,
      error: resp.error?.message ?? 'censorguardd 拒绝了请求',
      errorCode: resp.error?.code,
    };
  }
  const body = resp.result?.response;
  return {
    ok: body?.ok ?? true,
    error: body?.error,
    domain: body?.domain,
    daemonBootId: body?.daemon_boot_id,
    hooksHealthy: body?.hooks_healthy,
  };
}

// 便捷入口: 一次性 attach + 启动心跳, 返回 (client, stateMachine)。
// Bootstrap 启动时调一次, dispose 时 client.dispose()。
export async function startProtection(opts: AttachOptions): Promise<{
  client: AttachSelfClient;
  stateMachine: ProtectionStateMachine;
  result: AttachResult;
}> {
  const stateMachine = new ProtectionStateMachine();
  const client = new AttachSelfClient(opts);
  const result = await client.attachSelf();
  stateMachine.toProtected({
    domainId: result.domainId,
    domain: result.domain,
    group: result.group,
    version: result.version,
    daemonBootId: result.daemonBootId,
    hooksHealthy: result.hooksHealthy,
  });
  // 心跳触发重 attach 时, Bootstrap 负责重新调 client.attachSelf();
  // 这里只把状态切到 attaching, 真正重 attach 由 Bootstrap 处理
  client.startHeartbeat(stateMachine, () => {});
  return { client, stateMachine, result };
}
