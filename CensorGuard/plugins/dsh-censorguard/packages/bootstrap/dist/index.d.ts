import { type AttachResult, type ProtectionState } from '@censorguard/dsh-runtime';
import type { CensorguardReady } from './censorguardReady.js';
export interface BootstrapConfig {
    policyGroup: string;
    instanceHint?: string;
    dshSockPath?: string;
    heartbeatIntervalMs?: number;
    blockOnFailure?: boolean;
    initialRetryMs?: number;
    maxRetryMs?: number;
}
export declare class BootstrapCore implements CensorguardReady {
    private cfg;
    private client;
    private stateMachine;
    private reattaching;
    private disposed;
    private waitResolvers;
    constructor(cfg: Required<BootstrapConfig>);
    get state(): ProtectionState;
    subscribe(fn: (s: ProtectionState) => void): () => void;
    waitForReady(timeoutMs?: number): Promise<void>;
    /**
     * 启动 Bootstrap: attach + 启动心跳。
     * 失败时根据 blockOnFailure 决定是否 throw (默认 throw, 阻塞 DSH Ready)。
     */
    start(): Promise<AttachResult>;
    /** 重新 attach (daemon 重启后心跳触发) */
    reattach(): Promise<void>;
    /** HMR/stop/dispose: 只断开连接, 不调 untrack */
    dispose(): void;
    private tryAttach;
    private scheduleHeartbeat;
    private scheduleRetry;
}
export declare function createBootstrap(cfg: BootstrapConfig): BootstrapCore;
export * from './cordis.js';
//# sourceMappingURL=index.d.ts.map