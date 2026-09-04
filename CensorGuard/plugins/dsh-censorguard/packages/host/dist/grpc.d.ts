export interface RequestContext {
    origin?: number;
    dshInstanceId?: string;
    domainId?: number;
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
    policyYaml: string;
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
export interface SetSwitchesReply {
    enableFile: boolean;
    enableExec: boolean;
    enableNet: boolean;
    auditFile: boolean;
    auditExec: boolean;
    auditNet: boolean;
}
export interface SwitchUpdate {
    enableFile?: boolean;
    enableExec?: boolean;
    enableNet?: boolean;
    auditFile?: boolean;
    auditExec?: boolean;
    auditNet?: boolean;
}
export declare const RequestOrigin: {
    readonly DSH_WEB_UI: 1;
    readonly AGENT_TOOL: 2;
    readonly DSH_INTERNAL: 3;
};
export declare const EventKind: {
    readonly FILE: 1;
    readonly EXEC: 2;
    readonly NET: 3;
    readonly GUARD: 4;
};
export interface EventsRequest {
    domainIds?: number[];
    kinds?: number[];
    allowed?: boolean;
    context?: RequestContext;
}
export interface AuditEvent {
    ts: string;
    kind: number;
    op: number;
    pid: number;
    tgid: number;
    allowed: boolean;
    policyVersion: number;
    ruleVersion: number;
    comm: string;
    detail: string;
    args: string[];
    domainId: number;
    domain: string;
    sequence: number;
    daemonBootId: string;
    droppedBefore: number;
}
export interface EventStream {
    on(event: 'data', cb: (raw: Record<string, unknown>) => void): void;
    on(event: 'error', cb: (err: Error) => void): void;
    on(event: 'end', cb: () => void): void;
    cancel(): void;
}
export declare function toAuditEvent(r: Record<string, unknown>): AuditEvent;
export declare class GrpcClient {
    private readonly addr;
    private readonly protoPath;
    private client;
    private grpcLib;
    private disposed;
    constructor(addr: string, // 默认 127.0.0.1:50051
    protoPath: string);
    /** 懒加载 gRPC client (避免单元测试无 proto 时崩) */
    private getClient;
    /** 包装 gRPC 调用为 Promise (使用 cb 风格) */
    private call;
    /** Status: 全局状态 (roots/domains/reload_gen/config/daemon_boot_id/hooks_healthy) */
    status(ctx?: RequestContext): Promise<StatusReply>;
    /** GetPolicy: 单组只读 (gRPC 角色允许读任意组名, 写才受限) */
    getPolicy(name: string, ctx?: RequestContext): Promise<GetPolicyReply>;
    /** ValidatePolicy: 只编译校验, 不动数据面 */
    validatePolicy(name: string, policyYaml: string, ctx?: RequestContext): Promise<ValidatePolicyReply>;
    /** ApplyPolicy: 整组替换 (限 DSH 白名单组), dryRun=true 只校验 */
    applyPolicy(name: string, policyYaml: string, dryRun: boolean, ctx?: RequestContext): Promise<ApplyPolicyReply>;
    /** SetSwitches: 运行时开关切换 (只传要改的字段; 回执为切换后全量状态) */
    setSwitches(update: SwitchUpdate, ctx?: RequestContext): Promise<SetSwitchesReply>;
    /** SubscribeEvents: 服务端推流。返回可读流, 调用方监听 'data'/'error'/'end',
     *  断线后自行重订阅 (见 auditStream.ts)。
     *  与一元 call() 分开: 流式方法返回 ClientReadableStream 而非走回调。 */
    subscribeEvents(req: EventsRequest): Promise<EventStream>;
    dispose(): void;
    private camelizeStatus;
}
//# sourceMappingURL=grpc.d.ts.map