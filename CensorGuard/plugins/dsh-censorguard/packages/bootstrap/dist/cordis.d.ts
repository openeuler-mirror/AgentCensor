import { type BootstrapConfig } from './index.js';
interface CordisContext {
    /** 注册随插件卸载自动回收的副作用, fn 返回 disposer */
    effect(fn: () => () => void): void;
    /** 提供服务实例, inject 该名字的 entry 会等到它出现 */
    provide(name: string, value: unknown): void;
}
export declare const name = "censorguard-bootstrap";
export declare const inject: string[];
export type Config = BootstrapConfig;
/** apply Cordis 插件体: attach → 提供 censorguardReady → 注册 dispose */
export declare function apply(ctx: CordisContext, config?: BootstrapConfig): Promise<void>;
export {};
//# sourceMappingURL=cordis.d.ts.map