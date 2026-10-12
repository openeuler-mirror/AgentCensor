// AuditStream: Host 长期持有 gRPC SubscribeEvents 流, 为当前 DSH Domain 建
// 有界事件缓存, 断线自动重连, 明确报告断线和 daemon 丢失。
//
// 数据链路: eBPF ringbuf → daemon events.sock → censorguard-grpc
//           → 本类 (有界缓存) → Client 长轮询 audit.snapshot/audit.wait
//
// 缺口语义: 不制造"日志完整"错觉。
//   - daemon 侧丢失: Event.dropped_before > 0
//   - 传输断线: 丢失数量未知 (过滤流的全局 sequence 跳变不能推断丢失)
//   - daemon 重启: daemon_boot_id 变化
// 以上都会在缓存里留下 gap 标记条目, UI 原样展示。全链路无持久化 (实时观察窗)。

import {
  toAuditEvent,
  type AuditEvent,
  type EventStream,
  type EventsRequest,
} from './grpc.js';

// EventStreamSource 可注入的流来源 (GrpcClient 满足该接口;
// 单测/自检用假流注入, 与 hostCore 注入 GrpcClient 同一风格)
export interface EventStreamSource {
  subscribeEvents(req: EventsRequest): Promise<EventStream>;
}

// AuditEntry 缓存条目: 事件 或 缺口标记 (UI 展示 gap 用)
export type AuditEntry =
  | { type: 'event'; event: AuditEvent }
  | {
      type: 'gap';
      count: number; // 已知丢失条数 (未知 = 0, 如断线标记)
      reason: 'dropped' | 'daemon_restart' | 'stream_reconnect';
      atSequence: number; // 缺口出现位置 (其后第一条事件的 sequence)
      message: string;
    };

// AuditStreamState 流状态 (实时/断线/重连中)
export type AuditStreamState = 'idle' | 'live' | 'reconnecting' | 'disposed';

// AuditStreamStatus 顶部状态条
export interface AuditStreamStatus {
  state: AuditStreamState;
  daemonBootId: string;
  lastSequence: number;
  droppedCount: number; // daemon 显式报告的本订阅丢失数；过滤跳号不计入
  evictedCount: number; // 缓存淘汰数量
  cachedCount: number; // 当前缓存条数 (含 gap 标记)
  lastError: string; // 最近一次流错误, 空 = 无
}

// AuditFilter snapshot/wait 的基础过滤 (kinds/allowed;
// 更丰富的 pid/comm/目标/时间过滤由 Client 在拉到的数据上做)
export interface AuditFilter {
  kinds?: number[];
  allowed?: boolean;
}

// AuditStreamOptions 缓存与重连参数
export interface AuditStreamOptions {
  maxEntries?: number; // 默认 5000
  maxBytes?: number; // 默认 10 MiB
  reconnectInitialMs?: number; // 默认 500
  reconnectMaxMs?: number; // 默认 5000
  now?: () => number; // 测试注入时钟
}

// SnapshotResult snapshot/wait 的返回
export interface SnapshotResult {
  entries: AuditEntry[];
  lastSequence: number; // Host 已见最大 sequence；分页游标用已返回条目
  status: AuditStreamStatus;
}

const DEFAULT_MAX_ENTRIES = 5000;
const DEFAULT_MAX_BYTES = 10 * 1024 * 1024;
// 长轮询不超过 25 秒
export const MAX_WAIT_MS = 25_000;

interface Waiter {
  afterSequence: number;
  afterBootId: string;
  filter?: AuditFilter;
  limit: number;
  resolve: (r: SnapshotResult) => void;
  timer: ReturnType<typeof setTimeout>;
  signal?: AbortSignal;
  onAbort: () => void;
}

export class AuditStream {
  private entries: AuditEntry[] = [];
  private entriesBytes = 0;
  private evictedCount = 0;
  private droppedCount = 0;
  private lastSequence = 0;
  private lastBootId = '';
  private lastError = '';
  private state: AuditStreamState = 'idle';
  private stream: EventStream | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelay: number;
  private disposed = false;
  private waiters: Waiter[] = [];
  private subscriptionGeneration = 0;
  private reconnectMessage = '';

  private readonly maxEntries: number;
  private readonly maxBytes: number;
  private readonly reconnectInitialMs: number;
  private readonly reconnectMaxMs: number;

  constructor(
    private readonly source: EventStreamSource,
    // 订阅过滤 (Host 必须带 domainIds, 不订阅全机审计)
    private request: EventsRequest,
    opts: AuditStreamOptions = {},
  ) {
    this.maxEntries = opts.maxEntries ?? DEFAULT_MAX_ENTRIES;
    this.maxBytes = opts.maxBytes ?? DEFAULT_MAX_BYTES;
    this.reconnectInitialMs = opts.reconnectInitialMs ?? 500;
    this.reconnectMaxMs = opts.reconnectMaxMs ?? 5000;
    this.reconnectDelay = this.reconnectInitialMs;
  }

  /** setDomainIds bootstrap attach 后回填当前 DSH Domain, 触发重订阅 */
  setDomainIds(domainIds: number[]): void {
    if (JSON.stringify(this.request.domainIds) === JSON.stringify(domainIds)) return;
    this.request = { ...this.request, domainIds };
    this.subscriptionGeneration++;
    if (this.state === 'live' || this.state === 'reconnecting') {
      this.cancelStream();
      void this.subscribe();
    }
  }

  /** start 开始订阅 (不阻塞; 断线自动重连直到 dispose) */
  start(): void {
    if (this.disposed || this.state !== 'idle') return;
    void this.subscribe();
  }

  status(): AuditStreamStatus {
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
  snapshot(afterSequence: number, limit: number, filter?: AuditFilter, afterBootId = this.lastBootId): SnapshotResult {
    return {
      entries: this.collect(afterSequence, limit, filter, afterBootId),
      lastSequence: this.lastSequence,
      status: this.status(),
    };
  }

  /** wait 长轮询: 有新条目即返回, 否则等到 timeoutMs (上限 25 秒);
   *  dispose 时所有等待立即以空结果返回 */
  wait(
    afterSequence: number,
    limit: number,
    timeoutMs: number,
    filter?: AuditFilter,
    signal?: AbortSignal,
    afterBootId = this.lastBootId,
  ): Promise<SnapshotResult> {
    const immediate = this.collect(afterSequence, limit, filter, afterBootId);
    if (immediate.length > 0 || this.disposed || signal?.aborted) {
      return Promise.resolve({
        entries: immediate,
        lastSequence: this.lastSequence,
        status: this.status(),
      });
    }
    const capped = Math.min(Math.max(timeoutMs, 0), MAX_WAIT_MS);
    return new Promise<SnapshotResult>((resolve) => {
      const waiter: Waiter = {
        afterSequence,
        afterBootId,
        filter,
        limit,
        resolve,
        timer: setTimeout(() => {
          this.waiters = this.waiters.filter((w) => w !== waiter);
          signal?.removeEventListener('abort', waiter.onAbort);
          resolve(this.snapshot(afterSequence, limit, filter, afterBootId));
        }, capped),
        signal,
        onAbort: () => {
          clearTimeout(waiter.timer);
          this.waiters = this.waiters.filter((w) => w !== waiter);
          signal?.removeEventListener('abort', waiter.onAbort);
          resolve(this.snapshot(afterSequence, limit, filter, afterBootId));
        },
      };
      this.waiters.push(waiter);
      signal?.addEventListener('abort', waiter.onAbort, { once: true });
    });
  }

  dispose(): void {
    if (this.disposed) return;
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
      w.signal?.removeEventListener('abort', w.onAbort);
      w.resolve({ entries: [], lastSequence: this.lastSequence, status: this.status() });
    }
  }

  // ============ 内部: 订阅与重连 ============

  private async subscribe(): Promise<void> {
    // 已有活跃流则不再订阅: 防止并发 subscribe 造成流增殖
    if (this.disposed || this.stream) return;
    const generation = this.subscriptionGeneration;
    let stream: EventStream;
    try {
      stream = await this.source.subscribeEvents(this.request);
    } catch (e) {
      if (generation === this.subscriptionGeneration) this.onStreamFailure(e);
      else if (!this.disposed && !this.stream) void this.subscribe();
      return;
    }
    // await 期间可能有并发订阅胜出 (如 setDomainIds), 丢弃本次结果
    if (this.disposed || this.stream || generation !== this.subscriptionGeneration) {
      try {
        stream.cancel();
      } catch {
        /* 已取消 */
      }
      if (!this.disposed && !this.stream) void this.subscribe();
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
      if (this.stream === stream) this.onEvent(toAuditEvent(raw));
    });
    stream.on('error', (err) => {
      if (this.stream === stream) this.onStreamFailure(err);
    });
    stream.on('end', () => {
      if (this.stream === stream) {
        this.onStreamFailure(new Error('事件流被服务端结束'));
      }
    });
  }

  private onStreamFailure(e: unknown): void {
    if (this.disposed) return;
    this.cancelStream();
    this.state = 'reconnecting';
    this.lastError = e instanceof Error ? e.message : String(e);
    // 标记跟随下一条真实事件，不能凭空推进游标、跳过重连后的首条事件。
    this.reconnectMessage = `事件流曾断线，期间事件数量未知: ${this.lastError}`;
    this.notifyWaiters(true);
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

  private cancelStream(): void {
    if (this.stream) {
      const stream = this.stream;
      this.stream = null;
      try {
        stream.cancel();
      } catch {
        /* 已取消 */
      }
    }
  }

  // ============ 内部: 事件与缺口 ============

  private onEvent(ev: AuditEvent): void {
    // daemon 重启: boot ID 变化, 此前缓存的 sequence 不再可比
    if (this.lastBootId && ev.daemonBootId && ev.daemonBootId !== this.lastBootId) {
      this.entries = [];
      this.entriesBytes = 0;
      this.lastSequence = 0;
      this.droppedCount = 0;
      this.evictedCount = 0;
      this.push({
        type: 'gap',
        count: 0,
        reason: 'daemon_restart',
        atSequence: ev.sequence,
        message: `daemon 已重启 (boot ${this.lastBootId} → ${ev.daemonBootId}), 期间事件不可比`,
      });
    }
    if (this.reconnectMessage) {
      this.push({ type: 'gap', count: 0, reason: 'stream_reconnect',
        atSequence: ev.sequence, message: this.reconnectMessage });
      this.reconnectMessage = '';
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
    // sequence 是全局编号：其他 Domain、显示过滤都会跳号，不能当成丢失。
    if (ev.daemonBootId) this.lastBootId = ev.daemonBootId;
    if (ev.sequence > this.lastSequence) this.lastSequence = ev.sequence;
    this.push({ type: 'event', event: ev });
    this.notifyWaiters();
  }

  /** push 入缓存并按条数/字节双上限淘汰最旧 (先达任一限制即淘汰) */
  private push(entry: AuditEntry): void {
    this.entries.push(entry);
    this.entriesBytes += entry.type === 'event' ? entrySize(entry.event) : 64;
    while (this.entries.length > this.maxEntries || this.entriesBytes > this.maxBytes) {
      const old = this.entries.shift();
      if (!old) break;
      this.entriesBytes -= old.type === 'event' ? entrySize(old.event) : 64;
      this.evictedCount++;
    }
  }

  private collect(afterSequence: number, limit: number, filter?: AuditFilter, afterBootId = this.lastBootId): AuditEntry[] {
    const cursor = afterBootId && this.lastBootId && afterBootId !== this.lastBootId ? 0 : afterSequence;
    const out: AuditEntry[] = [];
    for (const e of this.entries) {
      const seq = e.type === 'event' ? e.event.sequence : e.atSequence;
      if (seq <= cursor) continue;
      // gap 与同序号事件必须整批返回，不能让页尾 gap 吞掉对应的 DENY。
      if (out.length >= Math.max(1, limit)) {
        const last = out[out.length - 1];
        const lastSeq = last.type === 'event' ? last.event.sequence : last.atSequence;
        if (seq !== lastSeq) break;
      }
      if (e.type === 'event' && filter) {
        if (filter.kinds && filter.kinds.length > 0 && !filter.kinds.includes(e.event.kind)) {
          continue;
        }
        if (filter.allowed != null && e.event.allowed !== filter.allowed) continue;
      }
      out.push(e);
    }
    return out;
  }

  private notifyWaiters(statusChanged = false): void {
    if (this.waiters.length === 0) return;
    const pending = this.waiters;
    this.waiters = [];
    for (const w of pending) {
      const entries = this.collect(w.afterSequence, w.limit, w.filter, w.afterBootId);
      if (entries.length === 0 && !this.disposed && !statusChanged) {
        this.waiters.push(w); // 仍无匹配条目, 继续等
        continue;
      }
      clearTimeout(w.timer);
      w.signal?.removeEventListener('abort', w.onAbort);
      w.resolve({ entries, lastSequence: this.lastSequence, status: this.status() });
    }
  }
}

// entrySize 事件近似字节数 (用于 10 MiB 上限; 不需要精确, JSON 长度足够)
function entrySize(ev: AuditEvent): number {
  return (
    64 +
    ev.ts.length +
    ev.comm.length +
    ev.detail.length +
    ev.domain.length +
    ev.daemonBootId.length +
    ev.args.reduce((n, a) => n + a.length + 8, 0)
  );
}
