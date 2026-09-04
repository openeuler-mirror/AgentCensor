// 审计会话页签定义 + 客户端事件缓冲
// 真实 DSH 集成时, ctx.slots.inject('conversation.view', AUDIT_TAB) 注册;
// 这里提供可独立测试的页签定义和缓冲逻辑。
//
// 数据链路: Client 不直连 gRPC, 通过 Host 的
//   audit.snapshot(afterSequence, limit, filters) / audit.wait(...) 长轮询
//   拉取条目 (lossless JSON), 本地缓冲去重后交给 auditView 渲染。

// ConversationViewTab DSH Client 注册的 conversation.view 结构
// 真实 DSH Cordis SDK 类型在 DSH 仓库集成时填, 这里只定义契约
export interface ConversationViewTab {
  name: string; // 'conversation.view'
  id: string; // 'security-audit'
  order: number; // 20: chat(0) / trajectory(10) 之后
  label: string; // '安全拦截审计'
  scope: 'session'; // 会话级页签
}

// 注册为第三个页签, 位于"对话""轨迹"右侧;
// 不占用 details Slot (那是工具调用详情的单占位区域)
export const AUDIT_TAB: ConversationViewTab = {
  name: 'conversation.view',
  id: 'security-audit',
  order: 20,
  label: '安全拦截审计',
  scope: 'session',
};

// ---------- Host → Client 的 RPC 数据形态 (lossless JSON) ----------
// 与 packages/host/src/auditStream.ts 的 AuditEntry/AuditStreamStatus 对齐;
// Client 不 import Host 包 (浏览器侧), 结构以 RPC JSON 为准。

export interface RpcAuditEvent {
  ts: string;
  kind: number; // 1=FILE 2=EXEC 3=NET 4=GUARD
  op: number;
  pid: number;
  tgid: number;
  allowed: boolean;
  policyVersion: number;
  ruleVersion: number;
  comm: string;
  detail: string;
  args: string[];
  domainId: number;
  domain: string;
  sequence: number;
  daemonBootId: string;
  droppedBefore: number;
}

export type RpcAuditEntry =
  | { type: 'event'; event: RpcAuditEvent }
  | {
      type: 'gap';
      count: number;
      reason: 'dropped' | 'sequence_jump' | 'daemon_restart' | 'stream_reconnect';
      atSequence: number;
      message: string;
    };

// RpcAuditStatus Host 流状态的 RPC 镜像 (顶部状态条用)
export interface RpcAuditStatus {
  state: 'idle' | 'live' | 'reconnecting' | 'disposed';
  daemonBootId: string;
  lastSequence: number;
  droppedCount: number; // 已知丢失事件数
  evictedCount: number; // Host 缓存淘汰数
  cachedCount: number;
  lastError: string;
}

export interface RpcAuditSnapshot {
  entries: RpcAuditEntry[];
  lastSequence: number;
  status: RpcAuditStatus;
}

// AuditBuffer Client 本地事件缓冲 (页签展示用, 有界去重)
// - 事件按 sequence 去重 (长轮询窗口可能重叠)
// - gap 标记按 atSequence+reason 去重
// - 超过上限淘汰最旧, 计数供状态条显示
export class AuditBuffer {
  private entries: RpcAuditEntry[] = [];
  private seenEvents = new Set<number>();
  private seenGaps = new Set<string>();
  private evicted = 0;

  constructor(private readonly maxEntries: number = 5000) {}

  /** ingest 合并一批 RPC 条目, 返回实际新增条数 */
  ingest(batch: RpcAuditEntry[]): number {
    let added = 0;
    for (const e of batch) {
      if (e.type === 'event') {
        if (this.seenEvents.has(e.event.sequence)) continue;
        this.seenEvents.add(e.event.sequence);
      } else {
        const key = `${e.reason}@${e.atSequence}`;
        if (this.seenGaps.has(key)) continue;
        this.seenGaps.add(key);
      }
      this.entries.push(e);
      added++;
    }
    while (this.entries.length > this.maxEntries) {
      const old = this.entries.shift();
      if (!old) break;
      if (old.type === 'event') this.seenEvents.delete(old.event.sequence);
      else this.seenGaps.delete(`${old.reason}@${old.atSequence}`);
      this.evicted++;
    }
    return added;
  }

  /** lastSequence 当前已见最大 sequence (下次 snapshot/wait 的 afterSequence) */
  lastSequence(): number {
    let max = 0;
    for (const e of this.entries) {
      const seq = e.type === 'event' ? e.event.sequence : e.atSequence;
      if (seq > max) max = seq;
    }
    return max;
  }

  all(): readonly RpcAuditEntry[] {
    return this.entries;
  }

  evictedCount(): number {
    return this.evicted;
  }

  get size(): number {
    return this.entries.length;
  }

  clear(): void {
    this.entries = [];
    this.seenEvents.clear();
    this.seenGaps.clear();
  }
}
