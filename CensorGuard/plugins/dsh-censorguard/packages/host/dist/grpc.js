// gRPC 客户端封装: Host 连本机 censorguard-grpc (默认 127.0.0.1:50051, loopback)
//
// 与 api/censorguard/v1/censorguard.proto 对齐 (proto 随包分发在
// packages/host/proto/ 下, 路径由 cordis.ts 解析传入)。
//
// 注意: 用动态 require 加载 @grpc/grpc-js 和 @grpc/proto-loader, 避免构建时
// 强依赖这两个包 (pnpm install 慢或网络受限时不阻塞 tsc 编译)
// RequestOrigin 枚举 (与 proto 一致; 仅审计与路径区分用, 不作授权依据)
export const RequestOrigin = {
    DSH_WEB_UI: 1,
    AGENT_TOOL: 2,
    DSH_INTERNAL: 3,
};
// EventKind 事件类型枚举 (proto 注释: 1=FILE 2=EXEC 3=NET 4=GUARD)
export const EventKind = {
    FILE: 1,
    EXEC: 2,
    NET: 3,
    GUARD: 4,
};
// toAuditEvent 把 grpc-js 返回的原始对象映射成 AuditEvent
// (proto-loader 开了 longs: String, uint64 回来是 string, 统一 Number())
export function toAuditEvent(r) {
    return {
        ts: String(r.ts ?? ''),
        kind: Number(r.kind ?? 0),
        op: Number(r.op ?? 0),
        pid: Number(r.pid ?? 0),
        tgid: Number(r.tgid ?? 0),
        allowed: Boolean(r.allowed),
        policyVersion: Number(r.policyVersion ?? 0),
        ruleVersion: Number(r.ruleVersion ?? 0),
        comm: String(r.comm ?? ''),
        detail: String(r.detail ?? ''),
        args: r.args ?? [],
        domainId: Number(r.domainId ?? 0),
        domain: String(r.domain ?? ''),
        sequence: Number(r.sequence ?? 0),
        daemonBootId: String(r.daemonBootId ?? ''),
        droppedBefore: Number(r.droppedBefore ?? 0),
    };
}
// GrpcClient Host 持有的 gRPC 客户端, 一次性创建长连接
export class GrpcClient {
    addr;
    protoPath;
    client = null;
    grpcLib = null;
    disposed = false;
    constructor(addr, // 默认 127.0.0.1:50051
    protoPath) {
        this.addr = addr;
        this.protoPath = protoPath;
    }
    /** 懒加载 gRPC client (避免单元测试无 proto 时崩) */
    async getClient() {
        if (this.disposed)
            throw new Error('GrpcClient 已 dispose');
        if (this.client)
            return this.client;
        // 动态 require @grpc/grpc-js 和 @grpc/proto-loader (ESM 兼容)
        // 不放在顶层 import, 避免构建时强依赖这两个包
        const { createRequire } = await import('node:module');
        const require_ = createRequire(import.meta.url);
        const grpcLib = require_('@grpc/grpc-js');
        const protoLoader = require_('@grpc/proto-loader');
        const def = protoLoader.loadSync(this.protoPath, {
            keepCase: false,
            longs: String,
            enums: String,
            defaults: true,
            oneofs: true,
        });
        const pkg = grpcLib.loadPackageDefinition(def);
        this.client = new pkg.censorguard.v1.Censorguard(this.addr, grpcLib.credentials.createInsecure());
        this.grpcLib = grpcLib;
        return this.client;
    }
    /** 包装 gRPC 调用为 Promise (使用 cb 风格) */
    async call(method, req) {
        const c = await this.getClient();
        const lib = this.grpcLib;
        const meta = new lib.Metadata();
        return new Promise((resolve, reject) => {
            const fn = c[method];
            if (typeof fn !== 'function') {
                reject(new Error(`gRPC 方法不存在: ${method}`));
                return;
            }
            fn.call(c, req, meta, (err, resp) => {
                if (err)
                    reject(err);
                else
                    resolve(resp);
            });
        });
    }
    /** Status: 全局状态 (roots/domains/reload_gen/config/daemon_boot_id/hooks_healthy) */
    async status(ctx) {
        const r = await this.call('Status', { context: ctx });
        return this.camelizeStatus(r);
    }
    /** GetPolicy: 单组只读 (gRPC 角色允许读任意组名, 写才受限) */
    async getPolicy(name, ctx) {
        const r = await this.call('GetPolicy', { name, context: ctx });
        return {
            name: String(r.name ?? ''),
            version: Number(r.version ?? 0),
            policyYaml: String(r.policyYaml ?? ''),
            rules: r.rules ?? [],
            domains: r.domains ?? [],
        };
    }
    /** ValidatePolicy: 只编译校验, 不动数据面 */
    async validatePolicy(name, policyYaml, ctx) {
        const r = await this.call('ValidatePolicy', {
            name,
            policyYaml,
            context: ctx,
        });
        return {
            ok: Boolean(r.ok),
            errors: r.errors ?? [],
            affected: r.affected ?? [],
        };
    }
    /** ApplyPolicy: 整组替换 (限 DSH 白名单组), dryRun=true 只校验 */
    async applyPolicy(name, policyYaml, dryRun, ctx) {
        const r = await this.call('ApplyPolicy', {
            name,
            policyYaml,
            dryRun,
            context: ctx,
        });
        return {
            reloadGen: Number(r.reloadGen ?? 0),
            name: String(r.name ?? ''),
            version: Number(r.version ?? 0),
            affected: r.affected ?? [],
        };
    }
    /** SetSwitches: 运行时开关切换 (只传要改的字段; 回执为切换后全量状态) */
    async setSwitches(update, ctx) {
        const r = await this.call('SetSwitches', {
            enableFile: update.enableFile,
            enableExec: update.enableExec,
            enableNet: update.enableNet,
            auditFile: update.auditFile,
            auditExec: update.auditExec,
            auditNet: update.auditNet,
            context: ctx,
        });
        return {
            enableFile: Boolean(r.enableFile),
            enableExec: Boolean(r.enableExec),
            enableNet: Boolean(r.enableNet),
            auditFile: Boolean(r.auditFile),
            auditExec: Boolean(r.auditExec),
            auditNet: Boolean(r.auditNet),
        };
    }
    /** SubscribeEvents: 服务端推流。返回可读流, 调用方监听 'data'/'error'/'end',
     *  断线后自行重订阅 (见 auditStream.ts)。
     *  与一元 call() 分开: 流式方法返回 ClientReadableStream 而非走回调。 */
    async subscribeEvents(req) {
        const c = await this.getClient();
        const lib = this.grpcLib;
        const meta = new lib.Metadata();
        const fn = c['SubscribeEvents'];
        if (typeof fn !== 'function') {
            throw new Error('gRPC 方法不存在: SubscribeEvents');
        }
        return fn.call(c, {
            domainIds: req.domainIds ?? [],
            kinds: req.kinds ?? [],
            allowed: req.allowed,
            context: req.context,
        }, meta);
    }
    dispose() {
        this.disposed = true;
        if (this.client) {
            try {
                this.client.close();
            }
            catch { /* 已关闭 */ }
            this.client = null;
        }
    }
    // grpc-js 默认返回 camelCase (我们开 keepCase=false), 这里手动映射成 TS 类型
    camelizeStatus(r) {
        const domains = (r.domains ?? []).map((d) => ({
            name: String(d.name ?? ''),
            id: Number(d.id ?? 0),
            slot: Number(d.slot ?? 0),
            group: String(d.group ?? ''),
            roots: Number(d.roots ?? 0),
            version: Number(d.version ?? 0),
            draining: Boolean(d.draining ?? false),
        }));
        return {
            roots: (r.roots ?? []),
            tracked: Number(r.tracked ?? 0),
            domains,
            reloadGen: Number(r.reloadGen ?? 0),
            lastReloadTime: String(r.lastReloadTime ?? ''),
            lastReloadError: String(r.lastReloadError ?? ''),
            config: (r.config ?? {}),
            daemonBootId: String(r.daemonBootId ?? ''),
            hooksHealthy: Boolean(r.hooksHealthy ?? false),
        };
    }
}
//# sourceMappingURL=grpc.js.map