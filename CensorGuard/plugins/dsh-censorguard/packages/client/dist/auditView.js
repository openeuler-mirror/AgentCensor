// 审计页签渲染逻辑: 纯函数, 把 RpcAuditEntry 映射成列表行/状态条/详情 JSON。
// 本文件只产出视图数据, 不接触 DOM。
export const AUDIT_EMPTY_TEXT = '当前 Domain 暂无审计事件';
export const AUDIT_DISCONNECTED_HINT = '事件流已断开, 列表保留已缓存事件; 断线期间的事件可能丢失 (见缺口标记)';
// kindLabel 把 kind/op 翻译成类型标签 (与 bpf/enforce.bpf.c 的
// FILE_OP_* / GUARD_OP_* 常量一致)
export function kindLabel(kind, op) {
    if (kind === 1 && op !== 0)
        return fileOpLabel(op);
    if (kind === 4 && op !== 0)
        return guardOpLabel(op);
    return { 1: 'FILE', 2: 'EXEC', 3: 'NET', 4: 'GUARD' }[kind] ?? '?';
}
// eventToRow 单条事件 → 列表行
export function eventToRow(ev) {
    return {
        time: ev.ts,
        result: ev.allowed ? 'ALLOW' : 'DENY',
        kind: ev.kind,
        kindLabel: kindLabel(ev.kind, ev.op),
        domain: ev.domain,
        domainId: ev.domainId,
        pid: ev.pid,
        tgid: ev.tgid,
        comm: ev.comm,
        target: ev.detail,
        args: ev.args,
        policyVersion: ev.policyVersion,
        ruleVersion: ev.ruleVersion,
        sequence: ev.sequence,
    };
}
// entriesToItems 缓冲条目 → 列表项 (事件成行, gap 成提示行)
// 渲染为最新在前: 缓冲保持正序追加 (lastSequence/去重依赖), 输出前整体倒序,
// 让最新事件出现在列表顶部; gap 行随其相邻事件一起倒置, 仍标记在正确位置
export function entriesToItems(entries) {
    return entries
        .slice()
        .reverse()
        .map((e) => e.type === 'event'
        ? { type: 'row', row: eventToRow(e.event) }
        : { type: 'gap', message: e.message, count: e.count });
}
// filterItems 应用第一期过滤; gap 提示行始终保留,
// 否则过滤后看不出中间丢过事件
export function filterItems(items, filter) {
    return items.filter((it) => {
        if (it.type === 'gap')
            return true;
        const r = it.row;
        if (filter.result && r.result !== filter.result)
            return false;
        if (filter.kinds && filter.kinds.length > 0 && !filter.kinds.includes(r.kind)) {
            return false;
        }
        if (filter.pid != null && r.pid !== filter.pid && r.tgid !== filter.pid)
            return false;
        if (filter.comm && r.comm !== filter.comm)
            return false;
        if (filter.target && !r.target.includes(filter.target))
            return false;
        if (filter.from && r.time < filter.from)
            return false;
        if (filter.to && r.time > filter.to)
            return false;
        if (filter.onlyDomainId != null && r.domainId !== filter.onlyDomainId)
            return false;
        return true;
    });
}
// statusBar 顶部状态条视图 (实时/断线/重连中 + boot ID +
// 最后 sequence + 已知丢失数 + 缓存淘汰数)
export function statusBar(status, localEvicted, listLength) {
    const connected = status.state === 'live';
    const stateLabel = status.state === 'live'
        ? '实时'
        : status.state === 'reconnecting'
            ? '重连中'
            : '已断开';
    return {
        stateLabel,
        connected,
        daemonBootId: status.daemonBootId,
        lastSequence: status.lastSequence,
        droppedCount: status.droppedCount,
        evictedCount: status.evictedCount + localEvicted,
        empty: listLength === 0,
    };
}
// eventDetailJson 选中事件的完整详情 JSON (审计页内部展示,
// 保留所有原始字段, 支持复制; 不复用全局 Details 单列)
export function eventDetailJson(ev) {
    return JSON.stringify(ev, null, 2);
}
// fileOpLabel FILE_OP_* 翻译 (与 bpf/enforce.bpf.c 的 FILE_OP_* 常量一致)
function fileOpLabel(op) {
    const labels = {
        1: 'FILE-OPEN-R',
        2: 'FILE-OPEN-W',
        3: 'FILE-TRUNC',
        4: 'FILE-UNLINK',
        5: 'FILE-RMDIR',
        6: 'FILE-RENAME',
        7: 'FILE-FTRUNC',
        8: 'FILE-LINK',
        9: 'FILE-SYMLINK',
        10: 'FILE-MKDIR',
        11: 'FILE-CHMOD',
        12: 'FILE-CHOWN',
        13: 'FILE-SETXATTR',
        14: 'FILE-MMAP-W',
        15: 'FILE-RMXATTR',
        16: 'FILE-SETACL',
        17: 'FILE-MKNOD',
        18: 'FILE-MPROTECT',
        19: 'FILE-FD-R',
        20: 'FILE-FD-W',
    };
    return labels[op] ?? 'FILE';
}
// guardOpLabel GUARD_OP_* 翻译 (与 bpf/enforce.bpf.c 的 GUARD_OP_* 常量一致)
function guardOpLabel(op) {
    const labels = {
        1: 'GUARD-KILL',
        2: 'GUARD-PTRACE',
        3: 'GUARD-TRACEME',
        4: 'GUARD-BPF',
    };
    return labels[op] ?? 'GUARD';
}
//# sourceMappingURL=auditView.js.map