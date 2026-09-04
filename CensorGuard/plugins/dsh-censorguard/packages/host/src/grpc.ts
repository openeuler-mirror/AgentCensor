// gRPC 客户端封装: Host 连本机 censorguard-grpc (默认 127.0.0.1:50051, loopback)
//
// 与 api/censorguard/v1/censorguard.proto 对齐 (proto 随包分发在
// packages/host/proto/ 下, 路径由 cordis.ts 解析传入)。
//
// 注意: 用动态 require 加载 @grpc/grpc-js 和 @grpc/proto-loader, 避免构建时
// 强依赖这两个包 (pnpm install 慢或网络受限时不阻塞 tsc 编译)

// 与 censorguard.proto 对齐的类型 (显式声明, 不依赖动态生成类型)
export interface RequestContext {
  origin?: number;            // RequestOrigin 枚举: 1=DSH_WEB_UI, 2=AGENT_TOOL, 3=DSH_INTERNAL
  dshInstanceId?: string;
  domainId?: number;          // 0 = 不指定
  sessionId?: string;
  toolCallId?: string;
}

export interface StatusReply {
  roots: number[];
  tracked: number;
  domains: DomainInfo[];
  reloadGen: number;
  lastReloadTime: string;
  lastReloadError: string;
  config: Record<string, boolean>;
  daemonBootId: string;
  hooksHealthy: boolean;
}

export interface DomainInfo {
  name: string;
  id: number;
  slot: number;
  group: string;
  roots: number;
  version: number;
  draining: boolean;
}

export interface GetPolicyReply {
  name: string;
  version: number;
  policyYaml: string;         // 组 YAML 片段原文 (编辑器直接展示/编辑)
  rules: string[];
  domains: string[];
}

export interface ValidatePolicyReply {
  ok: boolean;
  errors: string[];
  affected: string[];
}

export interface ApplyPolicyReply {
  reloadGen: number;
  name: string;
  version: number;
  affected: string[];
}

// SetSwitchesReply 运行时开关切换回执 (切换后的全量状态)
export interface SetSwitchesReply {
  enableFile: boolean;
  enableExec: boolean;
  enableNet: boolean;
  auditFile: boolean;
  auditExec: boolean;
  auditNet: boolean;
}

// SwitchUpdate 只传要改的开关 (undefined = 保持不变)
export interface SwitchUpdate {
  enableFile?: boolean;
  enableExec?: boolean;
  enableNet?: boolean;
  auditFile?: boolean;
  auditExec?: boolean;
  auditNet?: boolean;
}

// RequestOrigin 枚举 (与 proto 一致; 仅审计与路径区分用, 不作授权依据)
export const RequestOrigin = {
  DSH_WEB_UI: 1,
  AGENT_TOOL: 2,
  DSH_INTERNAL: 3,
} as const;

// EventKind 事件类型枚举 (proto 注释: 1=FILE 2=EXEC 3=NET 4=GUARD)
export const EventKind = {
  FILE: 1,
  EXEC: 2,
  NET: 3,
  GUARD: 4,
} as const;

// EventsRequest SubscribeEvents 请求 (过滤在 gRPC 侧完成, 空字段 = 不过滤;
// Host 应始终带 domainIds 订阅当前 DSH Domain)
export interface EventsRequest {
  domainIds?: number[];
  kinds?: number[];
  allowed?: boolean;
  context?: RequestContext;
}

// AuditEvent 审计事件 (proto Event 的 TS 形态, uint64 已转 number)
export interface AuditEvent {
  ts: string;               // RFC3339, daemon 收到事件的时刻
  kind: number;             // 1=FILE 2=EXEC 3=NET 4=GUARD
  op: number;               // kind=1 时 FILE_OP_*, kind=4 时 GUARD_OP_*
  pid: number;
  tgid: number;
  allowed: boolean;
  policyVersion: number;
  ruleVersion: number;
  comm: string;
  detail: string;           // 目标 (路径/IP)
  args: string[];           // EXEC 参数 token (kind=2 时有值)
  domainId: number;         // 0 = 未归属
  domain: string;           // daemon 解析的域名 (解析不出 = 空串)
  sequence: number;         // daemon 内单调序列
  daemonBootId: string;     // 区分 daemon 重启
  droppedBefore: number;    // 上次广播到现在的累计丢失数 (本订阅者)
}

// EventStream grpc-js ClientReadableStream 的最小接口
// (动态加载的库没有静态类型, 只声明用到的面)
export interface EventStream {
  on(event: 'data', cb: (raw: Record<string, unknown>) => void): void;
  on(event: 'error', cb: (err: Error) => void): void;
  on(event: 'end', cb: () => void): void;
  cancel(): void;
}

// toAuditEvent 把 grpc-js 返回的原始对象映射成 AuditEvent
// (proto-loader 开了 longs: String, uint64 回来是 string, 统一 Number())
export function toAuditEvent(r: Record<string, unknown>): AuditEvent {
  return {
    ts: String(r.ts ?? ''),
    kind: Number(r.kind ?? 0),
    op: Number(r.op ?? 0),
    pid: Number(r.pid ?? 0),
    tgid: Number(r.tgid ?? 0),
    allowed: Boolean(r.allowed),
    policyVersion: Number(r.policyVersion ?? 0),
    ruleVersion: Number(r.ruleVersion ?? 0),
    comm: String(r.comm ?? ''),
    detail: String(r.detail ?? ''),
    args: (r.args as string[]) ?? [],
    domainId: Number(r.domainId ?? 0),
    domain: String(r.domain ?? ''),
    sequence: Number(r.sequence ?? 0),
    daemonBootId: String(r.daemonBootId ?? ''),
    droppedBefore: Number(r.droppedBefore ?? 0),
  };
}

// 内部 grpc 库类型 (动态加载)
interface GrpcLib {
  credentials: { createInsecure(): unknown };
  loadPackageDefinition: (def: unknown) => unknown;
  Metadata: new () => { set(key: string, value: string): void };
}
interface ProtoLoaderLib {
  loadSync: (path: string, opts: unknown) => unknown;
}
type GrpcHandle = { close(): void } & Record<string, unknown>;

// GrpcClient Host 持有的 gRPC 客户端, 一次性创建长连接
export class GrpcClient {
  private client: GrpcHandle | null = null;
  private grpcLib: GrpcLib | null = null;
  private disposed = false;

  constructor(
    private readonly addr: string,       // 默认 127.0.0.1:50051
    private readonly protoPath: string,  // packages/host/proto/censorguard/v1/censorguard.proto
  ) {}

  /** 懒加载 gRPC client (避免单元测试无 proto 时崩) */
  private async getClient(): Promise<GrpcHandle> {
    if (this.disposed) throw new Error('GrpcClient 已 dispose');
    if (this.client) return this.client;

    // 动态 require @grpc/grpc-js 和 @grpc/proto-loader (ESM 兼容)
    // 不放在顶层 import, 避免构建时强依赖这两个包
    const { createRequire } = await import('node:module');
    const require_ = createRequire(import.meta.url);
    const grpcLib = require_('@grpc/grpc-js') as GrpcLib;
    const protoLoader = require_('@grpc/proto-loader') as ProtoLoaderLib;

    const def = protoLoader.loadSync(this.protoPath, {
      keepCase: false,
      longs: String,
      enums: String,
      defaults: true,
      oneofs: true,
    });
    const pkg = grpcLib.loadPackageDefinition(def) as {
      censorguard: { v1: { Censorguard: { new (addr: string, cred: unknown): GrpcHandle } } };
    };
    this.client = new pkg.censorguard.v1.Censorguard(this.addr, grpcLib.credentials.createInsecure());
    this.grpcLib = grpcLib;
    return this.client;
  }

  /** 包装 gRPC 调用为 Promise (使用 cb 风格) */
  private async call<T>(method: string, req: unknown): Promise<T> {
    const c = await this.getClient();
    const lib = this.grpcLib!;
    const meta = new lib.Metadata();
    return new Promise<T>((resolve, reject) => {
      const fn = c[method] as
        | ((m: unknown, r: unknown, cb: (e: Error | null, r2: T) => void) => void)
        | undefined;
      if (typeof fn !== 'function') {
        reject(new Error(`gRPC 方法不存在: ${method}`));
        return;
      }
      fn.call(c, req, meta, (err, resp) => {
        if (err) reject(err);
        else resolve(resp);
      });
    });
  }

  /** Status: 全局状态 (roots/domains/reload_gen/config/daemon_boot_id/hooks_healthy) */
  async status(ctx?: RequestContext): Promise<StatusReply> {
    const r = await this.call<Record<string, unknown>>('Status', { context: ctx });
    return this.camelizeStatus(r);
  }

  /** GetPolicy: 单组只读 (gRPC 角色允许读任意组名, 写才受限) */
  async getPolicy(name: string, ctx?: RequestContext): Promise<GetPolicyReply> {
    const r = await this.call<Record<string, unknown>>('GetPolicy', { name, context: ctx });
    return {
      name: String(r.name ?? ''),
      version: Number(r.version ?? 0),
      policyYaml: String(r.policyYaml ?? ''),
      rules: (r.rules as string[]) ?? [],
      domains: (r.domains as string[]) ?? [],
    };
  }

  /** ValidatePolicy: 只编译校验, 不动数据面 */
  async validatePolicy(name: string, policyYaml: string, ctx?: RequestContext): Promise<ValidatePolicyReply> {
    const r = await this.call<Record<string, unknown>>('ValidatePolicy', {
      name,
      policyYaml,
      context: ctx,
    });
    return {
      ok: Boolean(r.ok),
      errors: (r.errors as string[]) ?? [],
      affected: (r.affected as string[]) ?? [],
    };
  }

  /** ApplyPolicy: 整组替换 (限 DSH 白名单组), dryRun=true 只校验 */
  async applyPolicy(
    name: string,
    policyYaml: string,
    dryRun: boolean,
    ctx?: RequestContext,
  ): Promise<ApplyPolicyReply> {
    const r = await this.call<Record<string, unknown>>('ApplyPolicy', {
      name,
      policyYaml,
      dryRun,
      context: ctx,
    });
    return {
      reloadGen: Number(r.reloadGen ?? 0),
      name: String(r.name ?? ''),
      version: Number(r.version ?? 0),
      affected: (r.affected as string[]) ?? [],
    };
  }

  /** SetSwitches: 运行时开关切换 (只传要改的字段; 回执为切换后全量状态) */
  async setSwitches(update: SwitchUpdate, ctx?: RequestContext): Promise<SetSwitchesReply> {
    const r = await this.call<Record<string, unknown>>('SetSwitches', {
      enableFile: update.enableFile,
      enableExec: update.enableExec,
      enableNet: update.enableNet,
      auditFile: update.auditFile,
      auditExec: update.auditExec,
      auditNet: update.auditNet,
      context: ctx,
    });
    return {
      enableFile: Boolean(r.enableFile),
      enableExec: Boolean(r.enableExec),
      enableNet: Boolean(r.enableNet),
      auditFile: Boolean(r.auditFile),
      auditExec: Boolean(r.auditExec),
      auditNet: Boolean(r.auditNet),
    };
  }

  /** SubscribeEvents: 服务端推流。返回可读流, 调用方监听 'data'/'error'/'end',
   *  断线后自行重订阅 (见 auditStream.ts)。
   *  与一元 call() 分开: 流式方法返回 ClientReadableStream 而非走回调。 */
  async subscribeEvents(req: EventsRequest): Promise<EventStream> {
    const c = await this.getClient();
    const lib = this.grpcLib!;
    const meta = new lib.Metadata();
    const fn = c['SubscribeEvents'] as
      | ((r: unknown, m: unknown) => EventStream)
      | undefined;
    if (typeof fn !== 'function') {
      throw new Error('gRPC 方法不存在: SubscribeEvents');
    }
    return fn.call(c, {
      domainIds: req.domainIds ?? [],
      kinds: req.kinds ?? [],
      allowed: req.allowed,
      context: req.context,
    }, meta);
  }

  dispose(): void {
    this.disposed = true;
    if (this.client) {
      try { this.client.close(); } catch { /* 已关闭 */ }
      this.client = null;
    }
  }

  // grpc-js 默认返回 camelCase (我们开 keepCase=false), 这里手动映射成 TS 类型
  private camelizeStatus(r: Record<string, unknown>): StatusReply {
    const domains = ((r.domains ?? []) as Record<string, unknown>[]).map((d) => ({
      name: String(d.name ?? ''),
      id: Number(d.id ?? 0),
      slot: Number(d.slot ?? 0),
      group: String(d.group ?? ''),
      roots: Number(d.roots ?? 0),
      version: Number(d.version ?? 0),
      draining: Boolean(d.draining ?? false),
    }));
    return {
      roots: (r.roots ?? []) as number[],
      tracked: Number(r.tracked ?? 0),
      domains,
      reloadGen: Number(r.reloadGen ?? 0),
      lastReloadTime: String(r.lastReloadTime ?? ''),
      lastReloadError: String(r.lastReloadError ?? ''),
      config: (r.config ?? {}) as Record<string, boolean>,
      daemonBootId: String(r.daemonBootId ?? ''),
      hooksHealthy: Boolean(r.hooksHealthy ?? false),
    };
  }
}
