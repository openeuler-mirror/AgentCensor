import { type AuditEvent, type EventStream, type EventsRequest } from './grpc.js';
export interface EventStreamSource {
    subscribeEvents(req: EventsRequest): Promise<EventStream>;
}
export type AuditEntry = {
    type: 'event';
    event: AuditEvent;
} | {
    type: 'gap';
    count: number;
    reason: 'dropped' | 'sequence_jump' | 'daemon_restart' | 'stream_reconnect';
    atSequence: number;
    message: string;
};
export type AuditStreamState = 'idle' | 'live' | 'reconnecting' | 'disposed';
export interface AuditStreamStatus {
    state: AuditStreamState;
    daemonBootId: string;
    lastSequence: number;
    droppedCount: number;
    evictedCount: number;
    cachedCount: number;
    lastError: string;
}
export interface AuditFilter {
    kinds?: number[];
    allowed?: boolean;
}
export interface AuditStreamOptions {
    maxEntries?: number;
    maxBytes?: number;
    reconnectInitialMs?: number;
    reconnectMaxMs?: number;
    now?: () => number;
}
export interface SnapshotResult {
    entries: AuditEntry[];
    lastSequence: number;
    status: AuditStreamStatus;
}
export declare const MAX_WAIT_MS = 25000;
export declare class AuditStream {
    private readonly source;
    private request;
    private entries;
    private entriesBytes;
    private evictedCount;
    private droppedCount;
    private lastSequence;
    private lastBootId;
    private lastError;
    private state;
    private stream;
    private reconnectTimer;
    private reconnectDelay;
    private disposed;
    private waiters;
    private readonly maxEntries;
    private readonly maxBytes;
    private readonly reconnectInitialMs;
    private readonly reconnectMaxMs;
    constructor(source: EventStreamSource, request: EventsRequest, opts?: AuditStreamOptions);
    /** setDomainIds bootstrap attach 后回填当前 DSH Domain, 触发重订阅 */
    setDomainIds(domainIds: number[]): void;
    /** start 开始订阅 (不阻塞; 断线自动重连直到 dispose) */
    start(): void;
    status(): AuditStreamStatus;
    /** snapshot 一次性拉取 afterSequence 之后的条目 */
    snapshot(afterSequence: number, limit: number, filter?: AuditFilter): SnapshotResult;
    /** wait 长轮询: 有新条目即返回, 否则等到 timeoutMs (上限 25 秒);
     *  dispose 时所有等待立即以空结果返回 */
    wait(afterSequence: number, limit: number, timeoutMs: number, filter?: AuditFilter): Promise<SnapshotResult>;
    dispose(): void;
    private subscribe;
    private onStreamFailure;
    private cancelStream;
    private onEvent;
    /** push 入缓存并按条数/字节双上限淘汰最旧 (先达任一限制即淘汰) */
    private push;
    private collect;
    private notifyWaiters;
}
//# sourceMappingURL=auditStream.d.ts.map