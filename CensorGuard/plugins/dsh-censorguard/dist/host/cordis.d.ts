import type { IncomingMessage, ServerResponse } from 'node:http';
import type { ProtectionState } from '../runtime/index.js';
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
interface WebRouteLike {
    kind: 'prefix';
    path: string;
    handler: (req: IncomingMessage, res: ServerResponse) => void | Promise<void>;
}
interface CordisContext {
    effect(fn: () => (() => void) | (() => Promise<void>)): void;
    inject(services: readonly string[], fn: (ctx: CordisContext) => void): void;
    censorguardReady: CensorguardReadyHandle;
    connection: {
        requestRejection(req: IncomingMessage): number | undefined;
    };
    webServer: {
        register(route: WebRouteLike): () => void;
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