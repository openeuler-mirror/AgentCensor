// Bootstrap 插件主入口
// 启动后立即: 连 dsh.sock → attach_self → 校验回执 → 提供 censorguardReady。
// 心跳检测 daemon boot ID 变化时重新 attach; 失败时切 degraded 态。
//
// cordis.js 是真实 Cordis 插件入口 (name/inject/apply), DSH Loader 加载包
// main 时识别; 本文件保持可独立测试的核心逻辑导出。

import {
  AttachError,
  AttachSelfClient,
  ProtectionStateMachine,
  Sockets,
  type AttachResult,
  type ProtectionState,
} from '../runtime/index.js';
import type { CensorguardReady } from './censorguardReady.js';

// BootstrapConfig 插件配置 (Profile patch 的 config 字段形状)
export interface BootstrapConfig {
  policyGroup: string; // 必填: 默认 censorguard-dsh-default (必须在 daemon --dsh-self-groups 白名单)
  instanceHint?: string; // 多实例 DSH 区分用 (域名 dsh-<uid>-<tgid>-<starttime>-<hint>)
  dshSockPath?: string; // 默认 /run/censorguard/dsh.sock
  heartbeatIntervalMs?: number; // 默认 5000
  // 失败策略: attach 失败时是否阻塞 webserver 等 entry 进入 Ready (默认 false)
  // false = 仅记录日志, degraded 态照常提供 censorguardReady, DSH 正常启动,
  //         后台指数退避重试 (daemon 就绪后自动转 protected)
  // true  = DSH 不进入 Ready (fail-closed, 强保护语义)
  blockOnFailure?: boolean;
  // 重试参数
  initialRetryMs?: number; // 首次重试间隔 (默认 1000)
  maxRetryMs?: number; // 退避最大间隔 (默认 30000)
}

// BootstrapCore 与 Cordis ctx 无耦合的核心逻辑, 可独立测试
export class BootstrapCore implements CensorguardReady {
  private client: AttachSelfClient | null = null;
  private stateMachine = new ProtectionStateMachine();
  private reattaching = false;
  private disposed = false;
  private readyWaits = new Set<(error?: Error) => void>();
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private retryDelay: number;

  constructor(private cfg: Required<BootstrapConfig>) {
    this.retryDelay = cfg.initialRetryMs;
  }

  get state(): ProtectionState {
    return this.stateMachine.get();
  }

  subscribe(fn: (s: ProtectionState) => void): () => void {
    return this.stateMachine.subscribe(fn);
  }

  async waitForReady(timeoutMs = 10000): Promise<void> {
    if (this.disposed) throw new Error('censorguardReady 已 dispose');
    const isReady = (state: ProtectionState): boolean => state.kind === 'protected'
      || (state.kind === 'degraded' && !this.cfg.blockOnFailure);
    if (isReady(this.state)) return;
    return new Promise<void>((resolve, reject) => {
      let settled = false;
      let unsubscribe = (): void => {};
      const finish = (error?: Error): void => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        unsubscribe();
        this.readyWaits.delete(finish);
        if (error) reject(error); else resolve();
      };
      const timer = setTimeout(() => finish(new Error(`censorguardReady 等待超时 ${timeoutMs}ms`)), timeoutMs);
      this.readyWaits.add(finish);
      unsubscribe = this.stateMachine.subscribe((s) => {
        if (isReady(s)) finish();
      });
      if (settled) unsubscribe(); // subscribe immediately reports the current state
    });
  }

  /**
   * 启动 Bootstrap: attach + 启动心跳。
   * 失败时根据 blockOnFailure 决定是否 throw (默认 false = 不阻塞 DSH Ready,
   * 进 degraded 态并后台重试; true = throw, fail-closed)。
   * 注意: 非阻塞模式下本方法仍 throw (由调用方捕获记录), 重试已在后台排定。
   */
  async start(): Promise<AttachResult> {
    this.disposed = false;
    this.stateMachine.toAttaching();
    try {
      const result = await this.tryAttach();
      this.scheduleHeartbeat();
      this.resetRetry();
      return result;
    } catch (e) {
      const reason = e instanceof AttachError ? `${e.kind}: ${e.message}` : String(e);
      this.stateMachine.toDegraded(reason);
      if (this.cfg.blockOnFailure) {
        throw e;
      }
      // 非阻塞模式: 后台重试
      this.scheduleRetry();
      throw e; // 仍 throw, 调用方决定
    }
  }

  /** 重新 attach (daemon 重启后心跳触发) */
  async reattach(): Promise<void> {
    if (this.reattaching || this.disposed) return;
    this.reattaching = true;
    this.stateMachine.toAttaching();
    try {
      // dispose 旧 client (不调 untrack; 域由 daemon 按根进程退出回收)
      this.client?.dispose();
      this.client = new AttachSelfClient({
        dshSockPath: this.cfg.dshSockPath,
        policyGroup: this.cfg.policyGroup,
        instanceHint: this.cfg.instanceHint,
        heartbeatIntervalMs: this.cfg.heartbeatIntervalMs,
      });
      const result = await this.client.attachSelf();
      this.stateMachine.toProtected({
        domainId: result.domainId,
        domain: result.domain,
        group: result.group,
        version: result.version,
        daemonBootId: result.daemonBootId,
        hooksHealthy: result.hooksHealthy,
      });
      this.scheduleHeartbeat();
      this.resetRetry();
    } catch (e) {
      const reason = e instanceof AttachError ? `${e.kind}: ${e.message}` : String(e);
      this.stateMachine.toDegraded(`reattach 失败: ${reason}`);
      this.scheduleRetry();
    } finally {
      this.reattaching = false;
    }
  }

  /** HMR/stop/dispose: 只断开连接, 不调 untrack */
  dispose(): void {
    this.disposed = true;
    this.client?.dispose();
    this.client = null;
    this.resetRetry();
    this.readyWaits.forEach((finish) => finish(new Error('censorguardReady 已 dispose')));
  }

  private async tryAttach(): Promise<AttachResult> {
    this.client = new AttachSelfClient({
      dshSockPath: this.cfg.dshSockPath,
      policyGroup: this.cfg.policyGroup,
      instanceHint: this.cfg.instanceHint,
      heartbeatIntervalMs: this.cfg.heartbeatIntervalMs,
    });
    const result = await this.client.attachSelf();
    this.stateMachine.toProtected({
      domainId: result.domainId,
      domain: result.domain,
      group: result.group,
      version: result.version,
      daemonBootId: result.daemonBootId,
      hooksHealthy: result.hooksHealthy,
    });
    return result;
  }

  private scheduleHeartbeat(): void {
    if (!this.client) return;
    this.client.startHeartbeat(this.stateMachine, () => {
      // 心跳检测到 daemon 重启 / 失败: 触发重 attach
      void this.reattach();
    });
  }

  private scheduleRetry(): void {
    if (this.disposed || this.retryTimer) return;
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      void this.reattach();
    }, this.retryDelay);
    this.retryDelay = Math.min(this.retryDelay * 2, this.cfg.maxRetryMs);
  }

  private resetRetry(): void {
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.retryDelay = this.cfg.initialRetryMs;
  }
}

export function createBootstrap(cfg: BootstrapConfig): BootstrapCore {
  const required: Required<BootstrapConfig> = {
    policyGroup: cfg.policyGroup,
    instanceHint: cfg.instanceHint ?? '',
    dshSockPath: cfg.dshSockPath ?? Sockets.dsh,
    heartbeatIntervalMs: cfg.heartbeatIntervalMs ?? 5000,
    blockOnFailure: cfg.blockOnFailure ?? false,
    initialRetryMs: cfg.initialRetryMs ?? 1000,
    maxRetryMs: cfg.maxRetryMs ?? 30000,
  };
  return new BootstrapCore(required);
}

// 真实 Cordis 插件入口 (name/inject/apply), DSH Loader 加载包 main 时识别
export * from './cordis.js';
