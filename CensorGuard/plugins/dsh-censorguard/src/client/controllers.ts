// 浏览器半控制器: 组件不接触 ctx, 数据经控制器进出。
// 控制器在 slot 注册的 inject 回调里创建 (那里可以碰 ctx), 组件只拿
// controller 实例 + 用 useSyncExternalStore 订阅状态。
//
// 规则部分为 MOCK (见 policyEditor.ts): 编辑器展示/提交的都是服务端
// 返回的策略原文 (GetPolicyReply.policyYaml), 不做格式转换。

import { PolicyEditor } from '../ui/policyEditor.js';
import { SECTION_NOTICE, type ProtectionStatusView } from '../ui/securitySection.js';
import {
  AuditBuffer,
  type RpcAuditEntry,
  type RpcAuditEvent,
  type RpcAuditStatus,
} from '../ui/auditTab.js';
import {
  entriesToItems,
  filterItems,
  statusBar,
  type AuditFilterView,
  type AuditListItem,
  type AuditStatusBar,
} from '../ui/auditView.js';
import { callRead, callAdmin, RpcError, type ClientConnection } from './rpc.js';

// ---------- 镜像类型 (Host 侧 RPC 返回值; Client 不 import Host 包) ----------

interface StatusReply {
  daemonBootId: string;
  hooksHealthy: boolean;
  reloadGen: number;
  domains: { name: string; id: number; group: string; version: number }[];
}

interface GetPolicyReply {
  name: string;
  version: number;
  policyYaml: string; // 组 YAML 片段原文 (mock 编辑器直接展示/编辑)
}

interface ValidatePolicyReply {
  ok: boolean;
  errors: string[];
}

interface ApplyPolicyReply {
  version: number;
  reloadGen: number;
}

interface AuditSnapshot {
  entries: RpcAuditEntry[];
  lastSequence: number;
  status: RpcAuditStatus;
}

// ---------- 可订阅基类 ----------

class Store<S> {
  private listeners = new Set<() => void>();
  constructor(protected snapshot: S) {}
  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };
  getSnapshot = (): S => this.snapshot;
  protected set(next: S): void {
    this.snapshot = next;
    for (const fn of [...this.listeners]) fn();
  }
}

// ---------- 安全策略设置页控制器 ----------

export interface SecurityState {
  status: ProtectionStatusView | null;
  editor: ReturnType<PolicyEditor['get']>;
  diff: { added: string[]; removed: string[]; unchanged: number } | null;
  busy: boolean;
  message: string; // 操作反馈 (成功/错误)
  notice: string;
  canOperate: boolean; // 远程浏览器为 false
}

export class SecurityController extends Store<SecurityState> {
  readonly editor = new PolicyEditor();

  constructor(private readonly conn: ClientConnection) {
    super({
      status: null,
      editor: editor0.get(),
      diff: null,
      busy: false,
      message: '',
      notice: SECTION_NOTICE,
      canOperate: conn.isLoopback,
    });
  }

  /** 重新读取: 状态 + 当前策略组原文 ([重新读取]) */
  async refresh(): Promise<void> {
    this.set({ ...this.snapshot, busy: true, message: '' });
    try {
      const [status, policy] = await Promise.all([
        callRead<StatusReply>(this.conn, 'status'),
        callRead<GetPolicyReply>(this.conn, 'policy'),
      ]);
      // MOCK: 编辑器直接展示服务端 YAML 原文, 不拆 rules[]
      this.editor.loadCurrent(policy.policyYaml, policy.version);
      const dom = status.domains.find((d) => d.group === policy.name);
      const view: ProtectionStatusView = {
        daemonConnected: true,
        hooksHealthy: status.hooksHealthy,
        domainName: dom?.name ?? '',
        domainId: dom?.id ?? 0,
        boundGroup: policy.name,
        policyVersion: policy.version,
        ruleVersion: 0,
        reloadGen: status.reloadGen,
        attachAt: 0,
        daemonBootId: status.daemonBootId,
        auditStreamConnected: false,
        auditDroppedCount: 0,
      };
      this.set({
        ...this.snapshot,
        status: view,
        editor: this.editor.get(),
        diff: null,
        busy: false,
        message: '已重新读取',
      });
    } catch (e) {
      this.set({ ...this.snapshot, busy: false, message: errText(e) });
    }
  }

  setDraft(yaml: string): void {
    this.editor.setDraft(yaml);
    this.set({ ...this.snapshot, editor: this.editor.get() });
  }

  /** 查看差异 ([查看差异]) */
  showDiff(): void {
    this.set({ ...this.snapshot, diff: this.editor.computeDiff() });
  }

  /** 校验策略 ([校验策略], 只编译不动数据面) */
  async validate(): Promise<void> {
    this.set({ ...this.snapshot, busy: true, message: '' });
    try {
      const draft = this.editor.get().draftYaml;
      const syntaxErr = this.editor.staticYamlSyntaxCheck(draft);
      if (syntaxErr) {
        this.editor.setRuleDiagnostics([]);
        this.set({
          ...this.snapshot,
          editor: this.editor.get(),
          busy: false,
          message: `语法错误: ${syntaxErr}`,
        });
        return;
      }
      // MOCK: 草稿原文直接送服务端校验, 不做格式转换
      const r = await callAdmin<ValidatePolicyReply>(this.conn, 'policy/validate', {
        policyYaml: draft,
      });
      // 防御: 过滤空串错误项 (旧版 gRPC 成功时会带 [""] 空串, 会永久禁用保存)
      this.editor.setRuleDiagnostics(r.errors.filter((e) => e.trim() !== ''));
      this.set({
        ...this.snapshot,
        editor: this.editor.get(),
        busy: false,
        message: r.ok ? '校验通过' : `校验失败: ${r.errors.length} 条错误`,
      });
    } catch (e) {
      this.set({ ...this.snapshot, busy: false, message: errText(e) });
    }
  }

  /** 保存并重新加载 (expectedVersion 乐观锁 → Validate → Apply) */
  async apply(): Promise<void> {
    const req = this.editor.buildApplyRequest();
    if (!req) {
      this.set({ ...this.snapshot, message: '当前内容不可保存 (未修改/有诊断/超限)' });
      return;
    }
    this.set({ ...this.snapshot, busy: true, message: '' });
    try {
      const r = await callAdmin<ApplyPolicyReply>(this.conn, 'policy/apply', req);
      this.set({ ...this.snapshot, busy: false, message: `已生效: 版本 v${r.version}` });
      await this.refresh();
    } catch (e) {
      // 版本冲突: 提示重新读取
      this.set({ ...this.snapshot, busy: false, message: errText(e) });
    }
  }
}

// ---------- 审计页签控制器 ----------

export interface AuditState {
  items: AuditListItem[];
  bar: AuditStatusBar;
  filter: AuditFilterView;
  selected: RpcAuditEvent | null;
}

const EMPTY_STATUS: RpcAuditStatus = {
  state: 'idle',
  daemonBootId: '',
  lastSequence: 0,
  droppedCount: 0,
  evictedCount: 0,
  cachedCount: 0,
  lastError: '',
};

export class AuditController extends Store<AuditState> {
  private readonly buffer = new AuditBuffer();
  private rpcStatus: RpcAuditStatus = EMPTY_STATUS;
  private disposed = false;

  constructor(private readonly conn: ClientConnection) {
    super({ items: [], bar: statusBar(EMPTY_STATUS, 0, 0), filter: {}, selected: null });
  }

  /** start 长轮询循环: snapshot 起步 → wait 续推 */
  start(): void {
    void this.loop();
  }

  dispose(): void {
    this.disposed = true;
  }

  setFilter(filter: AuditFilterView): void {
    this.set({ ...this.snapshot, filter, items: this.visibleItems(filter) });
  }

  select(ev: RpcAuditEvent | null): void {
    this.set({ ...this.snapshot, selected: ev });
  }

  /** eventAt 按 sequence 取缓冲里的完整原始事件 (详情面板用) */
  eventAt(sequence: number): RpcAuditEvent | null {
    for (const e of this.buffer.all()) {
      if (e.type === 'event' && e.event.sequence === sequence) return e.event;
    }
    return null;
  }

  private async loop(): Promise<void> {
    // 起步: 全量 snapshot
    try {
      const snap = await callRead<AuditSnapshot>(this.conn, 'audit/snapshot', {
        afterSequence: 0,
        limit: 200,
      });
      this.ingest(snap);
    } catch {
      // 首拉失败也进入 wait 循环, 由状态条显示断线
    }
    while (!this.disposed) {
      try {
        const r = await callRead<AuditSnapshot>(this.conn, 'audit/wait', {
          afterSequence: this.buffer.lastSequence(),
          limit: 200,
          timeoutMs: 25000,
        });
        this.ingest(r);
      } catch {
        if (!this.disposed) await sleep(1000); // 传输失败退避后重试
      }
    }
  }

  private ingest(snap: AuditSnapshot): void {
    this.buffer.ingest(snap.entries);
    this.rpcStatus = snap.status;
    const items = this.visibleItems(this.snapshot.filter);
    this.set({
      ...this.snapshot,
      items,
      bar: statusBar(this.rpcStatus, this.buffer.evictedCount(), items.length),
    });
  }

  private visibleItems(filter: AuditFilterView): AuditListItem[] {
    return filterItems(entriesToItems(this.buffer.all()), filter);
  }
}

// ---------- 辅助 ----------

const editor0 = new PolicyEditor();

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

function errText(e: unknown): string {
  if (e instanceof RpcError) {
    if (e.kind === 'forbidden') return `无权操作: ${e.message}`;
    if (e.kind === 'version_conflict') return `版本冲突: ${e.message}`;
    return e.message;
  }
  return e instanceof Error ? e.message : String(e);
}
