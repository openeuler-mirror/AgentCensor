// Host 的真实 Cordis 插件入口
//
// DSH Loader 加载包 main 时识别这里的 name/inject/apply 导出:
//   - inject ['connection', 'censorguardReady']: 等 DSH Connection 服务和
//     Bootstrap 的 censorguardReady 都出现后才激活
//   - apply: 建 GrpcClient/HostCore/AuditStream, 再经
//     ctx.inject(['webServer'], ...) 在 Web Profile 里挂载两条 HTTP 前缀路由:
//       /censorguard-read   任何通过 trust fence 的 caller 可读
//                           (status/policy/audit 只读)
//       /censorguard-admin  仅 loopback (Host 头为 127/8、localhost、[::1])
//                           (validate/apply/switches 写)
//     trust fence 复用 Connection 的 requestRejection (公共 API); loopback
//     限定由本文件的 Host 头检查执行 (0.1.5 起 Connection 不再提供
//     按通道 authority), HostCore 侧另有组白名单兜底
//
// 传输形态说明 (dsh 0.1.5+): 上游 Connection 重构后, rpc.handle() 注册独立
// 通道的路径对第三方插件不可用 (其内部经 shadow ctx 访问 webServer, 而
// Connection 自身已不再 inject webServer), 共享 /api 通道的 intercept 又被
// api-gateway 独占。因此按第一方插件 (api-gateway WebSocket 路由) 的同款
// 模式, 直接在 webServer 上注册前缀路由, 并自行实现 RPC 信封 (与浏览器半
// conn.rpc.call 的请求/响应格式保持兼容)。
//
// ctx/connection/webServer 用最小结构化接口声明 (运行时是 DSH vendored
// cordis 与 @deepseek-ai/dsh-client-connection / dsh-host-webserver),
// 不在本包编译期引入这些依赖。
import { fileURLToPath } from 'node:url';
import { GrpcClient } from './grpc.js';
import { HostCore, HostError } from './hostCore.js';
import { AuditStream, MAX_WAIT_MS } from './auditStream.js';
export const name = 'censorguard-host';
// connection: trust fence 入口; censorguardReady: 拿当前 Domain 做事件订阅过滤
// (只订阅当前 DSH Domain, 不订阅全机审计)。webServer 刻意不放进硬 inject:
// 非 Web Profile 里没有 webServer 服务, 硬 inject 会让插件永远不激活;
// 第一方插件 (connection/api-gateway) 同样用 ctx.inject(['webServer'], ...)
// 做可选挂载。
export const inject = ['connection', 'censorguardReady'];
// 默认 proto 路径: 随包分发 (不写死绝对路径), 编译产物在 dist/, proto 在
// 包根 proto/ 下, 相对本文件解析。可用 config.protoPath 或
// CENSORGUARD_PROTO_PATH 环境变量覆盖。
const DEFAULT_PROTO_PATH = fileURLToPath(new URL('../../proto/censorguard/v1/censorguard.proto', import.meta.url));
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
        return () => {
            unsubscribeReady();
            audit.dispose();
            host.dispose();
        };
    });
    // webServer 只在 Web Profile 存在, 用 ctx.inject 做可选挂载 (同第一方插件)
    ctx.inject(['webServer'], (webCtx) => {
        webCtx.effect(() => {
            const unregisterRead = webCtx.webServer.register({
                kind: 'prefix',
                path: '/censorguard-read',
                handler: serveRpc(webCtx, '/censorguard-read', false, (endpoint, payload) => routeRead(host, audit, boundGroup, endpoint, payload)),
            });
            const unregisterAdmin = webCtx.webServer.register({
                kind: 'prefix',
                path: '/censorguard-admin',
                handler: serveRpc(webCtx, '/censorguard-admin', true, (endpoint, payload) => routeAdmin(host, boundGroup, endpoint, payload)),
            });
            console.log(`[censorguard-host] RPC 通道已注册: /censorguard-read (trust fence), ` +
                `/censorguard-admin (loopback only); grpc=${addr} group=${boundGroup}`);
            return () => {
                unregisterRead();
                unregisterAdmin();
            };
        });
    });
}
// ---------- HTTP/RPC 传输 ----------
// 与上游 Connection 保持一致的单请求体上限 (buffered JSON)
const MAX_BODY_BYTES = 8 * 1024 * 1024;
const ENDPOINT_SEGMENT_PATTERN = /^[A-Za-z0-9_$.-]+$/;
class BodyTooLargeError extends Error {
}
// 生成一条通道的 Node 风格 HTTP handler: 先过 Connection trust fence,
// admin 通道再要求 loopback Host 头, 然后按浏览器半 conn.rpc.call 的信封
// 格式 ({type:'client-request',rpcId,method,payload} →
// {type:'server-response',rpcId,result}) 解码/派发/回包
function serveRpc(ctx, channel, loopbackOnly, handler) {
    return async (req, res) => {
        const rejection = ctx.connection.requestRejection(req);
        if (rejection !== undefined) {
            res.writeHead(rejection).end(rejection === 401 ? 'unauthorized' : 'forbidden');
            return;
        }
        if (loopbackOnly && !isLoopbackAuthority(req.headers.host)) {
            res.writeHead(403).end('forbidden');
            return;
        }
        const endpoint = endpointFromChannel(channel, req.url ?? '');
        if (req.method !== 'POST' || endpoint === undefined) {
            res.writeHead(404).end('not found');
            return;
        }
        const mediaType = req.headers['content-type']?.split(';', 1)[0]?.trim().toLowerCase();
        if (mediaType !== 'application/json') {
            res.writeHead(415).end('content type must be application/json');
            return;
        }
        let body;
        try {
            body = JSON.parse(await readBody(req));
        }
        catch (e) {
            if (e instanceof BodyTooLargeError) {
                res.writeHead(413).end('request body too large');
            }
            else {
                res.writeHead(400).end('body is not JSON');
            }
            return;
        }
        const message = body;
        if (message === null || typeof message !== 'object' ||
            message.type !== 'client-request' ||
            typeof message.rpcId !== 'string' ||
            typeof message.method !== 'string') {
            writeEnvelope(res, '', {
                ok: false,
                error: { code: 'internal', message: 'bad_request: invalid client-request message', details: {} },
            });
            return;
        }
        if (message.method !== endpoint) {
            writeEnvelope(res, message.rpcId, {
                ok: false,
                error: {
                    code: 'internal',
                    message: `bad_request: method ${JSON.stringify(message.method)} does not match endpoint ${JSON.stringify(endpoint)}`,
                    details: {},
                },
            });
            return;
        }
        // 客户端断开 (含长轮询 audit/wait 被 AbortSignal 取消) 时中断 handler
        const ac = new AbortController();
        res.on('close', () => {
            if (!res.writableEnded)
                ac.abort();
        });
        try {
            const result = await handler(endpoint, message.payload, ac.signal);
            writeEnvelope(res, message.rpcId, result);
        }
        catch (e) {
            if (!res.headersSent)
                res.writeHead(500).end(`handler failure: ${String(e)}`);
            else
                res.end();
        }
    };
}
function writeEnvelope(res, rpcId, result) {
    const body = JSON.stringify({ type: 'server-response', rpcId, result });
    res.writeHead(200, { 'content-type': 'application/json' }).end(body);
}
async function readBody(req) {
    const chunks = [];
    let size = 0;
    for await (const chunk of req) {
        size += chunk.length;
        if (size > MAX_BODY_BYTES)
            throw new BodyTooLargeError('request body too large');
        chunks.push(chunk);
    }
    return Buffer.concat(chunks).toString('utf8');
}
// 与上游 endpointFromPath 对齐: 去掉查询串, 剥掉通道前缀, 逐段校验
function endpointFromChannel(channel, url) {
    const pathname = url.split('?', 1)[0];
    if (!pathname.startsWith(`${channel}/`))
        return undefined;
    const endpoint = pathname.slice(channel.length + 1);
    const segments = endpoint.split('/');
    if (segments.some((segment) => segment === '' || segment === '.' || segment === '..' || !ENDPOINT_SEGMENT_PATTERN.test(segment))) {
        return undefined;
    }
    return endpoint;
}
// 与上游 isLoopbackHostname 对齐: localhost、IPv6 loopback、IPv4 127/8。
// Host 头是 DNS rebinding 无法伪造的 (trust fence 同样以它为准),
// 远程浏览器经 LAN IP 访问时 Host 为 LAN IP, 在此被拒
function isLoopbackAuthority(authority) {
    if (!authority)
        return false;
    let hostname;
    try {
        hostname = new URL(`http://${authority}`).hostname;
    }
    catch {
        return false;
    }
    if (hostname === 'localhost' || hostname === '[::1]')
        return true;
    const parts = hostname.split('.');
    return (parts.length === 4 &&
        parts[0] === '127' &&
        parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255));
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
            // loopback 限定已由 serveRpc 的 Host 头检查执行;
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