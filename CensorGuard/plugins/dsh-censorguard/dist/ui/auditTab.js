// 审计会话页签定义 + 客户端事件缓冲
// 真实 DSH 集成时, ctx.slots.inject('conversation.view', AUDIT_TAB) 注册;
// 这里提供可独立测试的页签定义和缓冲逻辑。
//
// 数据链路: Client 不直连 gRPC, 通过 Host 的
//   audit.snapshot(afterSequence, limit, filters) / audit.wait(...) 长轮询
//   拉取条目 (lossless JSON), 本地缓冲去重后交给 auditView 渲染。
// 注册为第三个页签, 位于"对话""轨迹"右侧;
// 不占用 details Slot (那是工具调用详情的单占位区域)
export const AUDIT_TAB = {
    name: 'conversation.view',
    id: 'security-audit',
    order: 20,
    label: '安全拦截审计',
    scope: 'session',
};
// AuditBuffer Client 本地事件缓冲 (页签展示用, 有界去重)
// - 事件按 sequence 去重 (长轮询窗口可能重叠)
// - gap 标记按 atSequence+reason 去重
// - 超过上限淘汰最旧, 计数供状态条显示
export class AuditBuffer {
    maxEntries;
    entries = [];
    seenEvents = new Set();
    seenGaps = new Set();
    evicted = 0;
    constructor(maxEntries = 5000) {
        this.maxEntries = maxEntries;
    }
    /** ingest 合并一批 RPC 条目, 返回实际新增条数 */
    ingest(batch) {
        let added = 0;
        for (const e of batch) {
            if (e.type === 'event') {
                if (this.seenEvents.has(e.event.sequence))
                    continue;
                this.seenEvents.add(e.event.sequence);
            }
            else {
                const key = `${e.reason}@${e.atSequence}`;
                if (this.seenGaps.has(key))
                    continue;
                this.seenGaps.add(key);
            }
            this.entries.push(e);
            added++;
        }
        while (this.entries.length > this.maxEntries) {
            const old = this.entries.shift();
            if (!old)
                break;
            if (old.type === 'event')
                this.seenEvents.delete(old.event.sequence);
            else
                this.seenGaps.delete(`${old.reason}@${old.atSequence}`);
            this.evicted++;
        }
        return added;
    }
    /** lastSequence 当前已见最大 sequence (下次 snapshot/wait 的 afterSequence) */
    lastSequence() {
        let max = 0;
        for (const e of this.entries) {
            const seq = e.type === 'event' ? e.event.sequence : e.atSequence;
            if (seq > max)
                max = seq;
        }
        return max;
    }
    all() {
        return this.entries;
    }
    evictedCount() {
        return this.evicted;
    }
    get size() {
        return this.entries.length;
    }
    clear() {
        this.entries = [];
        this.seenEvents.clear();
        this.seenGaps.clear();
    }
}
//# sourceMappingURL=auditTab.js.map