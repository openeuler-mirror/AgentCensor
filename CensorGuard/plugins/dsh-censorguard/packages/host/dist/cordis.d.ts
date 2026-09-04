import type { ProtectionState } from '@censorguard/dsh-runtime';
export interface HostPluginConfig {
    grpcAddr?: string;
    protoPath?: string;
    boundGroup?: string;
}
export type RpcResult = {
    ok: true;
    value: unknown;
} | {
    ok: false;
    error: {
        code: 'internal';
        message: string;
        details: Record<string, never>;
    };
};
type RpcHandler = (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<RpcResult>;
interface CordisContext {
    effect(fn: () => () => void): void;
    censorguardReady: CensorguardReadyHandle;
    connection: {
        rpc: {
            handle(channel: string, handler: RpcHandler, options: {
                authority: 'trusted-host' | 'loopback';
            }): () => Promise<void>;
        };
    };
}
interface CensorguardReadyHandle {
    readonly state: ProtectionState;
    subscribe(fn: (s: ProtectionState) => void): () => void;
}
export declare const name = "censorguard-host";
export declare const inject: string[];
export declare function apply(ctx: CordisContext, config?: HostPluginConfig): void;
export {};
//# sourceMappingURL=cordis.d.ts.map