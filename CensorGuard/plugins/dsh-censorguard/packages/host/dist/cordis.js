// Host 的真实 Cordis 插件入口
//
// DSH Loader 加载包 main 时识别这里的 name/inject/apply 导出:
//   - inject ['connection', 'censorguardReady']: 等 DSH Connection 服务和
//     Bootstrap 的 censorguardReady 都出现后才激活
//   - apply: 建 GrpcClient/HostCore/AuditStream, 通过
//     ctx.connection.rpc.handle() 注册两条 RPC 通道:
//       /censorguard-read   authority: trusted-host  (status/policy/audit 只读)
//       /censorguard-admin  authority: loopback      (validate/apply 写)
//     authority 由 DSH Connection 的 trust fence 执行 (远程浏览器写请求在
//     进入 handler 前就被拒), HostCore.assertLoopback 作为第二层兜底
//
// ctx/connection 用最小结构化接口声明 (运行时是 DSH vendored cordis 与
// @deepseek-ai/dsh-client-connection), 不在本包编译期引入这些依赖。
import { fileURLToPath } from 'node:url';
import { GrpcClient } from './grpc.js';
import { HostCore, HostError } from './hostCore.js';
import { AuditStream, MAX_WAIT_MS } from './auditStream.js';
export const name = 'censorguard-host';
// connection: RPC 通道注册点; censorguardReady: 拿当前 Domain 做事件订阅过滤
// (只订阅当前 DSH Domain, 不订阅全机审计)
export const inject = ['connection', 'censorguardReady'];
// 默认 proto 路径: 随包分发 (不写死绝对路径), 编译产物在 dist/, proto 在
// 包根 proto/ 下, 相对本文件解析。可用 config.protoPath 或
// CENSORGUARD_PROTO_PATH 环境变量覆盖。
const DEFAULT_PROTO_PATH = fileURLToPath(new URL('../proto/censorguard/v1/censorguard.proto', import.meta.url));
export function apply(ctx, config) {
    const addr = config?.grpcAddr ?? '127.0.0.1:50051';
    const protoPath = config?.protoPath ?? process.env.CENSORGUARD_PROTO_PATH ?? DEFAULT_PROTO_PATH;
    const boundGroup = config?.boundGroup ?? 'censorguard-dsh-default';
    const grpc = new GrpcClient(addr, protoPath);
    const host = new HostCore(grpc, boundGroup);
    // 事件订阅过滤先为空 (attach 未完成时收不到本 Domain 事件, 但不会误订全机:
    // 回填 domainIds 前的窗口内按不过滤订阅, Domain 归属在 Client 展示层标注)
    const audit = new AuditStream(grpc, {});
    // Bootstrap 状态 → HostCore.domainId + AuditStream 订阅过滤
    const ready = ctx.censorguardReady;
    const applyState = (s) => {
        if (s.kind === 'protected') {
            host.setDomainId(s.domainId);
            audit.setDomainIds([s.domainId]);
        }
    };
    applyState(ready.state);
    const unsubscribeReady = ready.subscribe(applyState);
    ctx.effect(() => {
        audit.start();
        const disposeRead = ctx.connection.rpc.handle('/censorguard-read', (endpoint, payload) => routeRead(host, audit, boundGroup, endpoint, payload), { authority: 'trusted-host' });
        const disposeAdmin = ctx.connection.rpc.handle('/censorguard-admin', (endpoint, payload) => routeAdmin(host, boundGroup, endpoint, payload), { authority: 'loopback' });
        console.log(`[censorguard-host] RPC 通道已注册: /censorguard-read (trusted-host), ` +
            `/censorguard-admin (loopback); grpc=${addr} group=${boundGroup}`);
        return () => {
            unsubscribeReady();
            void disposeRead();
            void disposeAdmin();
            audit.dispose();
            host.dispose();
        };
    });
}
// ---------- 路由 ----------
async function routeRead(host, audit, boundGroup, endpoint, payload) {
    try {
        switch (endpoint) {
            case 'status':
                return ok(await host.readStatus());
            case 'policy': {
                const p = (payload ?? {});
                // group 省略/为空时默认当前绑定组 (Client 不需要知道组名)
                return ok(await host.readPolicy(p.group || boundGroup));
            }
            case 'audit/status':
                return ok(audit.status());
            case 'audit/snapshot': {
                const p = (payload ?? {});
                return ok(audit.snapshot(p.afterSequence ?? 0, capLimit(p.limit), p.filter));
            }
            case 'audit/wait': {
                const p = (payload ?? {});
                // 长轮询不超过 25 秒 (AuditStream.wait 内建上限)
                return ok(await audit.wait(p.afterSequence ?? 0, capLimit(p.limit), p.timeoutMs ?? MAX_WAIT_MS, p.filter));
            }
            default:
                return errResult(new Error(`未知 read 端点: ${endpoint}`));
        }
    }
    catch (e) {
        return errResult(e);
    }
}
async function routeAdmin(host, boundGroup, endpoint, payload) {
    try {
        const p = (payload ?? {});
        // group 省略/为空时默认当前绑定组 (Client 不需要知道组名; 非绑定组仍被
        // HostCore.assertEditableGroup 拒)
        const group = p.group || boundGroup;
        switch (endpoint) {
            // authority: loopback 已由 Connection trust fence 执行;
            // HostCore 的 assertLoopback 传固定 loopback 值, 组白名单校验仍生效
            case 'policy/validate':
                return ok(await host.validatePolicy({ group, policyYaml: p.policyYaml ?? '' }, '127.0.0.1'));
            case 'policy/apply':
                return ok(await host.applyPolicy({ group, policyYaml: p.policyYaml ?? '', expectedVersion: p.expectedVersion }, '127.0.0.1'));
            case 'switches/set':
                return ok(await host.setSwitches({
                    enableFile: p.enableFile,
                    enableExec: p.enableExec,
                    enableNet: p.enableNet,
                    auditFile: p.auditFile,
                    auditExec: p.auditExec,
                    auditNet: p.auditNet,
                }, '127.0.0.1'));
            default:
                return errResult(new Error(`未知 admin 端点: ${endpoint}`));
        }
    }
    catch (e) {
        return errResult(e);
    }
}
function capLimit(limit) {
    // 单次拉取上限 1000, 防浏览器一次性拿全量缓存
    return Math.min(Math.max(limit ?? 200, 1), 1000);
}
function ok(value) {
    return { ok: true, value };
}
// HostError 的业务分类 (forbidden/version_conflict/bad_request) 放 message
// 前缀, Client 按前缀还原提示 (RpcError code 是封闭联合, 通用兜底只有 internal)
function errResult(e) {
    if (e instanceof HostError) {
        return {
            ok: false,
            error: { code: 'internal', message: `${e.kind}: ${e.message}`, details: {} },
        };
    }
    return {
        ok: false,
        error: {
            code: 'internal',
            message: e instanceof Error ? e.message : String(e),
            details: {},
        },
    };
}
//# sourceMappingURL=cordis.js.map