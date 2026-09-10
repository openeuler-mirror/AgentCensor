export interface ConversationViewTab {
    name: string;
    id: string;
    order: number;
    label: string;
    scope: 'session';
}
export declare const AUDIT_TAB: ConversationViewTab;
export interface RpcAuditEvent {
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
export type RpcAuditEntry = {
    type: 'event';
    event: RpcAuditEvent;
} | {
    type: 'gap';
    count: number;
    reason: 'dropped' | 'sequence_jump' | 'daemon_restart' | 'stream_reconnect';
    atSequence: number;
    message: string;
};
export interface RpcAuditStatus {
    state: 'idle' | 'live' | 'reconnecting' | 'disposed';
    daemonBootId: string;
    lastSequence: number;
    droppedCount: number;
    evictedCount: number;
    cachedCount: number;
    lastError: string;
}
export interface RpcAuditSnapshot {
    entries: RpcAuditEntry[];
    lastSequence: number;
    status: RpcAuditStatus;
}
export declare class AuditBuffer {
    private readonly maxEntries;
    private entries;
    private seenEvents;
    private seenGaps;
    private evicted;
    constructor(maxEntries?: number);
    /** ingest 合并一批 RPC 条目, 返回实际新增条数 */
    ingest(batch: RpcAuditEntry[]): number;
    /** lastSequence 当前已见最大 sequence (下次 snapshot/wait 的 afterSequence) */
    lastSequence(): number;
    all(): readonly RpcAuditEntry[];
    evictedCount(): number;
    get size(): number;
    clear(): void;
}
//# sourceMappingURL=auditTab.d.ts.map