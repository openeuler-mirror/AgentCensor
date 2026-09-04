// AuditStream: Host 长期持有 gRPC SubscribeEvents 流, 为当前 DSH Domain 建
// 有界事件缓存, 断线自动重连, sequence gap 可见。
//
// 数据链路: eBPF ringbuf → daemon events.sock → censorguard-grpc
//           → 本类 (有界缓存) → Client 长轮询 audit.snapshot/audit.wait
//
// 缺口语义: 不制造"日志完整"错觉。
//   - daemon 侧丢失: Event.dropped_before > 0
//   - 传输断线丢失: 重连后 sequence 跳变
//   - daemon 重启: daemon_boot_id 变化
// 以上都会在缓存里留下 gap 标记条目, UI 原样展示。全链路无持久化 (实时观察窗)。
import { toAuditEvent, } from './grpc.js';
const DEFAULT_MAX_ENTRIES = 5000;
const DEFAULT_MAX_BYTES = 10 * 1024 * 1024;
// 长轮询不超过 25 秒
export const MAX_WAIT_MS = 25_000;
export class AuditStream {
    source;
    request;
    entries = [];
    entriesBytes = 0;
    evictedCount = 0;
    droppedCount = 0;
    lastSequence = 0;
    lastBootId = '';
    lastError = '';
    state = 'idle';
    stream = null;
    reconnectTimer = null;
    reconnectDelay;
    disposed = false;
    waiters = [];
    maxEntries;
    maxBytes;
    reconnectInitialMs;
    reconnectMaxMs;
    constructor(source, 
    // 订阅过滤 (Host 必须带 domainIds, 不订阅全机审计)
    request, opts = {}) {
        this.source = source;
        this.request = request;
        this.maxEntries = opts.maxEntries ?? DEFAULT_MAX_ENTRIES;
        this.maxBytes = opts.maxBytes ?? DEFAULT_MAX_BYTES;
        this.reconnectInitialMs = opts.reconnectInitialMs ?? 500;
        this.reconnectMaxMs = opts.reconnectMaxMs ?? 5000;
        this.reconnectDelay = this.reconnectInitialMs;
    }
    /** setDomainIds bootstrap attach 后回填当前 DSH Domain, 触发重订阅 */
    setDomainIds(domainIds) {
        this.request = { ...this.request, domainIds };
        if (this.state === 'live' || this.state === 'reconnecting') {
            this.cancelStream();
            void this.subscribe();
        }
    }
    /** start 开始订阅 (不阻塞; 断线自动重连直到 dispose) */
    start() {
        if (this.disposed || this.state !== 'idle')
            return;
        void this.subscribe();
    }
    status() {
        return {
            state: this.state,
            daemonBootId: this.lastBootId,
            lastSequence: this.lastSequence,
            droppedCount: this.droppedCount,
            evictedCount: this.evictedCount,
            cachedCount: this.entries.length,
            lastError: this.lastError,
        };
    }
    /** snapshot 一次性拉取 afterSequence 之后的条目 */
    snapshot(afterSequence, limit, filter) {
        return {
            entries: this.collect(afterSequence, limit, filter),
            lastSequence: this.lastSequence,
            status: this.status(),
        };
    }
    /** wait 长轮询: 有新条目即返回, 否则等到 timeoutMs (上限 25 秒);
     *  dispose 时所有等待立即以空结果返回 */
    wait(afterSequence, limit, timeoutMs, filter) {
        const immediate = this.collect(afterSequence, limit, filter);
        if (immediate.length > 0 || this.disposed) {
            return Promise.resolve({
                entries: immediate,
                lastSequence: this.lastSequence,
                status: this.status(),
            });
        }
        const capped = Math.min(Math.max(timeoutMs, 0), MAX_WAIT_MS);
        return new Promise((resolve) => {
            const waiter = {
                afterSequence,
                filter,
                limit,
                resolve,
                timer: setTimeout(() => {
                    this.waiters = this.waiters.filter((w) => w !== waiter);
                    resolve(this.snapshot(afterSequence, limit, filter));
                }, capped),
            };
            this.waiters.push(waiter);
        });
    }
    dispose() {
        if (this.disposed)
            return;
        this.disposed = true;
        this.state = 'disposed';
        this.cancelStream();
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        // 插件 dispose 时中止所有长轮询
        const pending = this.waiters;
        this.waiters = [];
        for (const w of pending) {
            clearTimeout(w.timer);
            w.resolve({ entries: [], lastSequence: this.lastSequence, status: this.status() });
        }
    }
    // ============ 内部: 订阅与重连 ============
    async subscribe() {
        // 已有活跃流则不再订阅: 防止并发 subscribe 造成流增殖
        if (this.disposed || this.stream)
            return;
        let stream;
        try {
            stream = await this.source.subscribeEvents(this.request);
        }
        catch (e) {
            this.onStreamFailure(e);
            return;
        }
        // await 期间可能有并发订阅胜出 (如 setDomainIds), 丢弃本次结果
        if (this.disposed || this.stream) {
            try {
                stream.cancel();
            }
            catch {
                /* 已取消 */
            }
            return;
        }
        this.stream = stream;
        this.state = 'live';
        this.lastError = '';
        this.reconnectDelay = this.reconnectInitialMs;
        // 新流已就绪, 作废还在排队的重连定时器
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        // 所有回调先校验流身份: 已被替换/主动取消的旧流, 其事件一律忽略。
        // grpc-js 的 cancel() 会给本流补发一个 CANCELLED error,
        // 不做身份校验会把它当成新断线, 叠加出指数级重连定时器与流。
        stream.on('data', (raw) => {
            if (this.stream === stream)
                this.onEvent(toAuditEvent(raw));
        });
        stream.on('error', (err) => {
            if (this.stream === stream)
                this.onStreamFailure(err);
        });
        stream.on('end', () => {
            if (this.stream === stream) {
                this.onStreamFailure(new Error('事件流被服务端结束'));
            }
        });
    }
    onStreamFailure(e) {
        if (this.disposed)
            return;
        this.cancelStream();
        this.state = 'reconnecting';
        this.lastError = e instanceof Error ? e.message : String(e);
        // 断线标记: 让 UI 列表能看到断流位置 (丢失数待重连后由 sequence 跳变补记)
        this.push({
            type: 'gap',
            count: 0,
            reason: 'stream_reconnect',
            atSequence: this.lastSequence + 1,
            message: `事件流断线, 重连中: ${this.lastError}`,
        });
        this.notifyWaiters();
        // 关键: 先清旧定时器再排新的, 任何时刻至多一个重连调度。
        // 不清的话同一次断线的 error+end 双回调会各排一个定时器,
        // 适配器不可达期间每个定时器触发的失败再各排一个, 指数增殖直到 OOM。
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
        }
        this.reconnectTimer = setTimeout(() => {
            this.reconnectTimer = null;
            void this.subscribe();
        }, this.reconnectDelay);
        this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.reconnectMaxMs);
    }
    cancelStream() {
        if (this.stream) {
            try {
                this.stream.cancel();
            }
            catch {
                /* 已取消 */
            }
            this.stream = null;
        }
    }
    // ============ 内部: 事件与缺口 ============
    onEvent(ev) {
        // daemon 重启: boot ID 变化, 此前缓存的 sequence 不再可比
        if (this.lastBootId && ev.daemonBootId && ev.daemonBootId !== this.lastBootId) {
            this.push({
                type: 'gap',
                count: 0,
                reason: 'daemon_restart',
                atSequence: ev.sequence,
                message: `daemon 已重启 (boot ${this.lastBootId} → ${ev.daemonBootId}), 期间事件不可比`,
            });
        }
        // daemon 侧广播丢失 (ringbuf 满 / 订阅者慢)
        if (ev.droppedBefore > 0) {
            this.droppedCount += ev.droppedBefore;
            this.push({
                type: 'gap',
                count: ev.droppedBefore,
                reason: 'dropped',
                atSequence: ev.sequence,
                message: `daemon 报告丢失 ${ev.droppedBefore} 条事件`,
            });
        }
        // 传输断线期间的丢失: sequence 跳变 (同 boot 才可比)
        if (this.lastSequence > 0 &&
            ev.sequence > this.lastSequence + 1 &&
            (!this.lastBootId || !ev.daemonBootId || ev.daemonBootId === this.lastBootId)) {
            const missed = ev.sequence - this.lastSequence - 1;
            this.droppedCount += missed;
            this.push({
                type: 'gap',
                count: missed,
                reason: 'sequence_jump',
                atSequence: ev.sequence,
                message: `事件序列跳变: ${this.lastSequence} → ${ev.sequence}, 丢失 ${missed} 条`,
            });
        }
        if (ev.daemonBootId)
            this.lastBootId = ev.daemonBootId;
        if (ev.sequence > this.lastSequence)
            this.lastSequence = ev.sequence;
        this.push({ type: 'event', event: ev });
        this.notifyWaiters();
    }
    /** push 入缓存并按条数/字节双上限淘汰最旧 (先达任一限制即淘汰) */
    push(entry) {
        this.entries.push(entry);
        this.entriesBytes += entry.type === 'event' ? entrySize(entry.event) : 64;
        while (this.entries.length > this.maxEntries || this.entriesBytes > this.maxBytes) {
            const old = this.entries.shift();
            if (!old)
                break;
            this.entriesBytes -= old.type === 'event' ? entrySize(old.event) : 64;
            this.evictedCount++;
        }
    }
    collect(afterSequence, limit, filter) {
        const out = [];
        for (const e of this.entries) {
            const seq = e.type === 'event' ? e.event.sequence : e.atSequence;
            if (seq <= afterSequence)
                continue;
            if (e.type === 'event' && filter) {
                if (filter.kinds && filter.kinds.length > 0 && !filter.kinds.includes(e.event.kind)) {
                    continue;
                }
                if (filter.allowed != null && e.event.allowed !== filter.allowed)
                    continue;
            }
            out.push(e);
            if (out.length >= limit)
                break;
        }
        return out;
    }
    notifyWaiters() {
        if (this.waiters.length === 0)
            return;
        const pending = this.waiters;
        this.waiters = [];
        for (const w of pending) {
            const entries = this.collect(w.afterSequence, w.limit, w.filter);
            if (entries.length === 0 && !this.disposed) {
                this.waiters.push(w); // 仍无匹配条目, 继续等
                continue;
            }
            clearTimeout(w.timer);
            w.resolve({ entries, lastSequence: this.lastSequence, status: this.status() });
        }
    }
}
// entrySize 事件近似字节数 (用于 10 MiB 上限; 不需要精确, JSON 长度足够)
function entrySize(ev) {
    return (64 +
        ev.ts.length +
        ev.comm.length +
        ev.detail.length +
        ev.domain.length +
        ev.daemonBootId.length +
        ev.args.reduce((n, a) => n + a.length + 8, 0));
}
//# sourceMappingURL=auditStream.js.map