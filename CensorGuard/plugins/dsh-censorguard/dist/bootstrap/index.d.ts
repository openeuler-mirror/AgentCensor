import { type AttachResult, type ProtectionState } from '../runtime/index.js';
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
     * 失败时根据 blockOnFailure 决定是否 throw (默认 false = 不阻塞 DSH Ready,
     * 进 degraded 态并后台重试; true = throw, fail-closed)。
     * 注意: 非阻塞模式下本方法仍 throw (由调用方捕获记录), 重试已在后台排定。
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