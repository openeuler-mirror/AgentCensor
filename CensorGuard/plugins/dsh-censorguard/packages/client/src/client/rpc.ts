// 浏览器半 RPC 封装: 经 ctx.connection.rpc.call 调 Host 的两条通道。
// Host 返回 RpcResult, 业务错误在 error.message 里带 kind 前缀
// (forbidden: / version_conflict: / bad_request:), 这里还原成 Error。

// ClientConnection @deepseek-ai/dsh-client-connection 浏览器半的最小接口
export interface ClientConnection {
  readonly isLoopback: boolean;
  readonly rpc: {
    call(
      channel: string,
      endpoint: string,
      payload: unknown,
      signal?: AbortSignal,
    ): Promise<RpcResult>;
  };
}

export type RpcResult =
  | { ok: true; value: unknown }
  | { ok: false; error: { code: string; message: string } };

// RpcError 业务错误, kind 从 message 前缀还原 (forbidden 等)
export class RpcError extends Error {
  constructor(
    public readonly kind: string,
    message: string,
  ) {
    super(message);
  }
}

function unwrap(r: RpcResult): unknown {
  if (r.ok) return r.value;
  const m = r.error.message;
  const idx = m.indexOf(': ');
  const kind = idx > 0 ? m.slice(0, idx) : 'internal';
  return Promise.reject(new RpcError(kind, idx > 0 ? m.slice(idx + 2) : m));
}

/** callRead /censorguard-read (trusted-host, 任何 caller 可读) */
export async function callRead<T>(conn: ClientConnection, endpoint: string, payload?: unknown): Promise<T> {
  return (await unwrap(await conn.rpc.call('/censorguard-read', endpoint, payload ?? {}))) as T;
}

/** callAdmin /censorguard-admin (loopback only; 远程浏览器在 Connection
 *  trust fence 处即被拒) */
export async function callAdmin<T>(conn: ClientConnection, endpoint: string, payload?: unknown): Promise<T> {
  return (await unwrap(await conn.rpc.call('/censorguard-admin', endpoint, payload ?? {}))) as T;
}
