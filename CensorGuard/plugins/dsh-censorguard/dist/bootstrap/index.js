// Bootstrap 插件主入口
// 启动后立即: 连 dsh.sock → attach_self → 校验回执 → 提供 censorguardReady。
// 心跳检测 daemon boot ID 变化时重新 attach; 失败时切 degraded 态。
//
// cordis.js 是真实 Cordis 插件入口 (name/inject/apply), DSH Loader 加载包
// main 时识别; 本文件保持可独立测试的核心逻辑导出。
import { AttachError, AttachSelfClient, ProtectionStateMachine, Sockets, } from '../runtime/index.js';
// BootstrapCore 与 Cordis ctx 无耦合的核心逻辑, 可独立测试
export class BootstrapCore {
    cfg;
    client = null;
    stateMachine = new ProtectionStateMachine();
    reattaching = false;
    disposed = false;
    waitResolvers = new Set();
    constructor(cfg) {
        this.cfg = cfg;
    }
    get state() {
        return this.stateMachine.get();
    }
    subscribe(fn) {
        return this.stateMachine.subscribe(fn);
    }
    async waitForReady(timeoutMs = 10000) {
        if (this.state.kind === 'protected')
            return;
        return new Promise((resolve, reject) => {
            const timer = setTimeout(() => {
                reject(new Error(`censorguardReady 等待超时 ${timeoutMs}ms`));
            }, timeoutMs);
            const unsub = this.stateMachine.subscribe((s) => {
                if (s.kind === 'protected') {
                    clearTimeout(timer);
                    unsub();
                    resolve();
                }
                else if (s.kind === 'degraded' && !this.cfg.blockOnFailure) {
                    // 非阻塞模式 (默认): degraded 也算 ready (不阻塞 DSH)
                    clearTimeout(timer);
                    unsub();
                    resolve();
                }
            });
        });
    }
    /**
     * 启动 Bootstrap: attach + 启动心跳。
     * 失败时根据 blockOnFailure 决定是否 throw (默认 false = 不阻塞 DSH Ready,
     * 进 degraded 态并后台重试; true = throw, fail-closed)。
     * 注意: 非阻塞模式下本方法仍 throw (由调用方捕获记录), 重试已在后台排定。
     */
    async start() {
        this.disposed = false;
        this.stateMachine.toAttaching();
        try {
            const result = await this.tryAttach();
            this.scheduleHeartbeat();
            this.waitResolvers.forEach((r) => r());
            this.waitResolvers.clear();
            return result;
        }
        catch (e) {
            const reason = e instanceof AttachError ? `${e.kind}: ${e.message}` : String(e);
            this.stateMachine.toDegraded(reason);
            if (this.cfg.blockOnFailure) {
                throw e;
            }
            // 非阻塞模式: 后台重试
            this.scheduleRetry();
            throw e; // 仍 throw, 调用方决定
        }
    }
    /** 重新 attach (daemon 重启后心跳触发) */
    async reattach() {
        if (this.reattaching || this.disposed)
            return;
        this.reattaching = true;
        this.stateMachine.toAttaching();
        try {
            // dispose 旧 client (不调 untrack; 域由 daemon 按根进程退出回收)
            this.client?.dispose();
            this.client = new AttachSelfClient({
                dshSockPath: this.cfg.dshSockPath,
                policyGroup: this.cfg.policyGroup,
                instanceHint: this.cfg.instanceHint,
                heartbeatIntervalMs: this.cfg.heartbeatIntervalMs,
            });
            const result = await this.client.attachSelf();
            this.stateMachine.toProtected({
                domainId: result.domainId,
                domain: result.domain,
                group: result.group,
                version: result.version,
                daemonBootId: result.daemonBootId,
                hooksHealthy: result.hooksHealthy,
            });
            this.scheduleHeartbeat();
            this.waitResolvers.forEach((r) => r());
            this.waitResolvers.clear();
        }
        catch (e) {
            const reason = e instanceof AttachError ? `${e.kind}: ${e.message}` : String(e);
            this.stateMachine.toDegraded(`reattach 失败: ${reason}`);
            this.scheduleRetry();
        }
        finally {
            this.reattaching = false;
        }
    }
    /** HMR/stop/dispose: 只断开连接, 不调 untrack */
    dispose() {
        this.disposed = true;
        this.client?.dispose();
        this.client = null;
    }
    async tryAttach() {
        this.client = new AttachSelfClient({
            dshSockPath: this.cfg.dshSockPath,
            policyGroup: this.cfg.policyGroup,
            instanceHint: this.cfg.instanceHint,
            heartbeatIntervalMs: this.cfg.heartbeatIntervalMs,
        });
        const result = await this.client.attachSelf();
        this.stateMachine.toProtected({
            domainId: result.domainId,
            domain: result.domain,
            group: result.group,
            version: result.version,
            daemonBootId: result.daemonBootId,
            hooksHealthy: result.hooksHealthy,
        });
        return result;
    }
    scheduleHeartbeat() {
        if (!this.client)
            return;
        this.client.startHeartbeat(this.stateMachine, () => {
            // 心跳检测到 daemon 重启 / 失败: 触发重 attach
            void this.reattach();
        });
    }
    scheduleRetry() {
        if (this.disposed)
            return;
        const initial = this.cfg.initialRetryMs;
        const max = this.cfg.maxRetryMs;
        let delay = initial;
        const attempt = async () => {
            if (this.disposed)
                return;
            try {
                await this.reattach();
            }
            catch {
                // 退避重试
                delay = Math.min(delay * 2, max);
                setTimeout(() => void attempt(), delay);
            }
        };
        setTimeout(() => void attempt(), delay);
    }
}
export function createBootstrap(cfg) {
    const required = {
        policyGroup: cfg.policyGroup,
        instanceHint: cfg.instanceHint ?? '',
        dshSockPath: cfg.dshSockPath ?? Sockets.dsh,
        heartbeatIntervalMs: cfg.heartbeatIntervalMs ?? 5000,
        blockOnFailure: cfg.blockOnFailure ?? false,
        initialRetryMs: cfg.initialRetryMs ?? 1000,
        maxRetryMs: cfg.maxRetryMs ?? 30000,
    };
    return new BootstrapCore(required);
}
// 真实 Cordis 插件入口 (name/inject/apply), DSH Loader 加载包 main 时识别
export * from './cordis.js';
//# sourceMappingURL=index.js.map