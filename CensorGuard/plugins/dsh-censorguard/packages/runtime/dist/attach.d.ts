import { ProtectionStateMachine } from './state.js';
export interface AttachOptions {
    dshSockPath?: string;
    policyGroup: string;
    instanceHint?: string;
    seed?: boolean;
    heartbeatIntervalMs?: number;
    expectDaemonBootId?: string;
    timeoutMs?: number;
}
export interface AttachResult {
    domainId: number;
    domain: string;
    group: string;
    version: number;
    daemonBootId: string;
    hooksHealthy: boolean;
}
export declare class AttachSelfClient {
    private opts;
    private readonly sockPath;
    private readonly timeoutMs;
    private heartbeatTimer;
    private disposed;
    constructor(opts: Required<Pick<AttachOptions, 'policyGroup'>> & AttachOptions);
    /**
     * 执行一次 attach_self: 连 dsh.sock → 发请求 → 校验回执 → 返回 AttachResult。
     * 失败抛 AttachError, 调用方决定是否重试 / 转 degraded。
     */
    attachSelf(): Promise<AttachResult>;
    /**
     * 启动心跳守护: 周期 status_self 校验 daemon boot ID 未变化, 变化则触发重 attach。
     * 失败时通过状态机切 degraded 并回调 onReattachNeeded。
     */
    startHeartbeat(stateMachine: ProtectionStateMachine, onReattachNeeded: () => void): void;
    /** 只断开连接和定时器, 不调 untrack (域由 daemon 按根进程退出回收) */
    dispose(): void;
    private call;
}
export declare function startProtection(opts: AttachOptions): Promise<{
    client: AttachSelfClient;
    stateMachine: ProtectionStateMachine;
    result: AttachResult;
}>;
//# sourceMappingURL=attach.d.ts.map