export type ProtectionState = {
    kind: 'attaching';
} | {
    kind: 'protected';
    domainId: number;
    domain: string;
    group: string;
    version: number;
    daemonBootId: string;
    hooksHealthy: boolean;
    attachAt: number;
} | {
    kind: 'degraded';
    reason: string;
    since: number;
};
export type ProtectionListener = (state: ProtectionState) => void;
export declare class ProtectionStateMachine {
    private state;
    private listeners;
    get(): ProtectionState;
    subscribe(fn: ProtectionListener): () => void;
    /** 切到 protected 状态 (attach_self 成功) */
    toProtected(p: Omit<Extract<ProtectionState, {
        kind: 'protected';
    }>, 'kind' | 'attachAt'>): void;
    /** 切到 degraded 状态 (心跳失败 / daemon 重启 / hook 不健康) */
    toDegraded(reason: string): void;
    /** 重新进入 attaching (daemon 重启后重新 attach 前) */
    toAttaching(): void;
    private transition;
}
export declare function isProtected(s: ProtectionState): s is Extract<ProtectionState, {
    kind: 'protected';
}>;
//# sourceMappingURL=state.d.ts.map