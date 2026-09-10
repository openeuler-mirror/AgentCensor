// 审计页签渲染逻辑: 纯函数, 把 RpcAuditEntry 映射成列表行/状态条/详情 JSON。
// 本文件只产出视图数据, 不接触 DOM。

import type {
  RpcAuditEntry,
  RpcAuditEvent,
  RpcAuditStatus,
} from './auditTab.js';

// AuditRow 列表行 (展示字段)
export interface AuditRow {
  time: string; // daemon 接收时间 (RFC3339)
  result: 'ALLOW' | 'DENY';
  kind: number; // 1=FILE 2=EXEC 3=NET 4=GUARD (过滤用)
  kindLabel: string; // FILE / EXEC / NET / GUARD (含 op 细分, 如 FILE-OPEN-W)
  domain: string; // Domain 名称 (空串 = 未归属)
  domainId: number;
  pid: number;
  tgid: number;
  comm: string;
  target: string; // detail: 路径/IP
  args: string[]; // EXEC 参数 token
  policyVersion: number;
  ruleVersion: number;
  sequence: number;
}

// AuditListItem 列表项: 事件行 或 缺口提示行 (断线保留已有事件
// 并显示断线提示, 缺口以行间标记呈现)
export type AuditListItem =
  | { type: 'row'; row: AuditRow }
  | { type: 'gap'; message: string; count: number };

// AuditFilterView 第一期过滤条件
export interface AuditFilterView {
  result?: 'ALLOW' | 'DENY'; // 只看放行/拒绝
  kinds?: number[]; // 只看这些类型 (1=FILE 2=EXEC 3=NET 4=GUARD)
  pid?: number; // PID 或 TGID 命中
  comm?: string; // 命令名 (精确)
  target?: string; // 目标路径/IP (子串)
  from?: string; // 时间范围起 (RFC3339, 字符串比较即可)
  to?: string; // 时间范围止
  onlyDomainId?: number; // 只看当前 Domain
}

// AuditStatusBar 顶部状态条
export interface AuditStatusBar {
  stateLabel: string; // 实时 / 重连中 / 已断开
  connected: boolean;
  daemonBootId: string;
  lastSequence: number;
  droppedCount: number; // 已知丢失事件数
  evictedCount: number; // Host 缓存淘汰数 + 本地缓冲淘汰数
  empty: boolean; // 空状态 (没有事件时显示空状态文案)
}

export const AUDIT_EMPTY_TEXT = '当前 Domain 暂无审计事件';
export const AUDIT_DISCONNECTED_HINT =
  '事件流已断开, 列表保留已缓存事件; 断线期间的事件可能丢失 (见缺口标记)';

// kindLabel 把 kind/op 翻译成类型标签 (与 bpf/enforce.bpf.c 的
// FILE_OP_* / GUARD_OP_* 常量一致)
export function kindLabel(kind: number, op: number): string {
  if (kind === 1 && op !== 0) return fileOpLabel(op);
  if (kind === 4 && op !== 0) return guardOpLabel(op);
  return { 1: 'FILE', 2: 'EXEC', 3: 'NET', 4: 'GUARD' }[kind] ?? '?';
}

// eventToRow 单条事件 → 列表行
export function eventToRow(ev: RpcAuditEvent): AuditRow {
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
export function entriesToItems(entries: readonly RpcAuditEntry[]): AuditListItem[] {
  return entries
    .slice()
    .reverse()
    .map((e) =>
      e.type === 'event'
        ? { type: 'row', row: eventToRow(e.event) }
        : { type: 'gap', message: e.message, count: e.count },
    );
}

// filterItems 应用第一期过滤; gap 提示行始终保留,
// 否则过滤后看不出中间丢过事件
export function filterItems(items: AuditListItem[], filter: AuditFilterView): AuditListItem[] {
  return items.filter((it) => {
    if (it.type === 'gap') return true;
    const r = it.row;
    if (filter.result && r.result !== filter.result) return false;
    if (filter.kinds && filter.kinds.length > 0 && !filter.kinds.includes(r.kind)) {
      return false;
    }
    if (filter.pid != null && r.pid !== filter.pid && r.tgid !== filter.pid) return false;
    if (filter.comm && r.comm !== filter.comm) return false;
    if (filter.target && !r.target.includes(filter.target)) return false;
    if (filter.from && r.time < filter.from) return false;
    if (filter.to && r.time > filter.to) return false;
    if (filter.onlyDomainId != null && r.domainId !== filter.onlyDomainId) return false;
    return true;
  });
}

// statusBar 顶部状态条视图 (实时/断线/重连中 + boot ID +
// 最后 sequence + 已知丢失数 + 缓存淘汰数)
export function statusBar(
  status: RpcAuditStatus,
  localEvicted: number,
  listLength: number,
): AuditStatusBar {
  const connected = status.state === 'live';
  const stateLabel =
    status.state === 'live'
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
export function eventDetailJson(ev: RpcAuditEvent): string {
  return JSON.stringify(ev, null, 2);
}

// fileOpLabel FILE_OP_* 翻译 (与 bpf/enforce.bpf.c 的 FILE_OP_* 常量一致)
function fileOpLabel(op: number): string {
  const labels: Record<number, string> = {
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
function guardOpLabel(op: number): string {
  const labels: Record<number, string> = {
    1: 'GUARD-KILL',
    2: 'GUARD-PTRACE',
    3: 'GUARD-TRACEME',
    4: 'GUARD-BPF',
  };
  return labels[op] ?? 'GUARD';
}
