import type { RpcAuditEntry, RpcAuditEvent, RpcAuditStatus } from './auditTab.js';
export interface AuditRow {
    time: string;
    result: 'ALLOW' | 'DENY';
    kind: number;
    kindLabel: string;
    domain: string;
    domainId: number;
    pid: number;
    tgid: number;
    comm: string;
    target: string;
    args: string[];
    policyVersion: number;
    ruleVersion: number;
    sequence: number;
}
export type AuditListItem = {
    type: 'row';
    row: AuditRow;
} | {
    type: 'gap';
    message: string;
    count: number;
};
export interface AuditFilterView {
    result?: 'ALLOW' | 'DENY';
    kinds?: number[];
    pid?: number;
    comm?: string;
    target?: string;
    from?: string;
    to?: string;
    onlyDomainId?: number;
}
export interface AuditStatusBar {
    stateLabel: string;
    connected: boolean;
    daemonBootId: string;
    lastSequence: number;
    droppedCount: number;
    evictedCount: number;
    empty: boolean;
}
export declare const AUDIT_EMPTY_TEXT = "\u5F53\u524D Domain \u6682\u65E0\u5BA1\u8BA1\u4E8B\u4EF6";
export declare const AUDIT_DISCONNECTED_HINT = "\u4E8B\u4EF6\u6D41\u5DF2\u65AD\u5F00, \u5217\u8868\u4FDD\u7559\u5DF2\u7F13\u5B58\u4E8B\u4EF6; \u65AD\u7EBF\u671F\u95F4\u7684\u4E8B\u4EF6\u53EF\u80FD\u4E22\u5931 (\u89C1\u7F3A\u53E3\u6807\u8BB0)";
export declare function kindLabel(kind: number, op: number): string;
export declare function eventToRow(ev: RpcAuditEvent): AuditRow;
export declare function entriesToItems(entries: readonly RpcAuditEntry[]): AuditListItem[];
export declare function filterItems(items: AuditListItem[], filter: AuditFilterView): AuditListItem[];
export declare function statusBar(status: RpcAuditStatus, localEvicted: number, listLength: number): AuditStatusBar;
export declare function eventDetailJson(ev: RpcAuditEvent): string;
//# sourceMappingURL=auditView.d.ts.map