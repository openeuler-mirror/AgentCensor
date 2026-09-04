import type { ProtectionState } from '@censorguard/dsh-runtime';
export interface CensorguardReady {
    /** 当前保护状态 (attaching/protected/degraded) */
    readonly state: ProtectionState;
    /** 订阅状态变化 */
    subscribe(fn: (s: ProtectionState) => void): () => void;
    /** 等待进入 protected 态 (Bootstrap 内部用; consumer 应通过 inject 等待) */
    waitForReady(timeoutMs?: number): Promise<void>;
    /** 触发重新 attach (daemon 重启后) */
    reattach(): Promise<void>;
}
export declare const CENSORGUARD_READY_KEY = "censorguardReady";
//# sourceMappingURL=censorguardReady.d.ts.map