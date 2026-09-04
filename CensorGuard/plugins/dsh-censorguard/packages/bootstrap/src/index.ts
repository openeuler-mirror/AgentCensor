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
} from '@censorguard/dsh-runtime';
import type { CensorguardReady } from './censorguardReady.js';

// BootstrapConfig 插件配置 (Profile patch 的 config 字段形状)
export interface BootstrapConfig {
  policyGroup: string; // 必填: 默认 censorguard-dsh-default (必须在 daemon --dsh-self-groups 白名单)
  instanceHint?: string; // 多实例 DSH 区分用 (域名 dsh-<uid>-<tgid>-<starttime>-<hint>)
  dshSockPath?: string; // 默认 /run/censorguard/dsh.sock
  heartbeatIntervalMs?: number; // 默认 5000
  // 失败策略: attach 失败时是否阻塞 webserver 等 entry 进入 Ready (默认 true)
  // true = DSH 不进入 Ready (fail-closed); false = 仅记录日志, degraded 进入 Ready (测试用)
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
  private waitResolvers = new Set<() => void>();

  constructor(private cfg: Required<BootstrapConfig>) {}

  get state(): ProtectionState {
    return this.stateMachine.get();
  }

  subscribe(fn: (s: ProtectionState) => void): () => void {
    return this.stateMachine.subscribe(fn);
  }

  async waitForReady(timeoutMs = 10000): Promise<void> {
    if (this.state.kind === 'protected') return;
    return new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(new Error(`censorguardReady 等待超时 ${timeoutMs}ms`));
      }, timeoutMs);
      const unsub = this.stateMachine.subscribe((s) => {
        if (s.kind === 'protected') {
          clearTimeout(timer);
          unsub();
          resolve();
        } else if (s.kind === 'degraded' && !this.cfg.blockOnFailure) {
          // 测试模式: degraded 也算 ready (不阻塞 DSH)
          clearTimeout(timer);
          unsub();
          resolve();
        }
      });
    });
  }

  /**
   * 启动 Bootstrap: attach + 启动心跳。
   * 失败时根据 blockOnFailure 决定是否 throw (默认 throw, 阻塞 DSH Ready)。
   */
  async start(): Promise<AttachResult> {
    this.disposed = false;
    this.stateMachine.toAttaching();
    try {
      const result = await this.tryAttach();
      this.scheduleHeartbeat();
      this.waitResolvers.forEach((r) => r());
      this.waitResolvers.clear();
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
      this.waitResolvers.forEach((r) => r());
      this.waitResolvers.clear();
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
    if (this.disposed) return;
    const initial = this.cfg.initialRetryMs;
    const max = this.cfg.maxRetryMs;
    let delay = initial;
    const attempt = async (): Promise<void> => {
      if (this.disposed) return;
      try {
        await this.reattach();
      } catch {
        // 退避重试
        delay = Math.min(delay * 2, max);
        setTimeout(() => void attempt(), delay);
      }
    };
    setTimeout(() => void attempt(), delay);
  }
}

export function createBootstrap(cfg: BootstrapConfig): BootstrapCore {
  const required: Required<BootstrapConfig> = {
    policyGroup: cfg.policyGroup,
    instanceHint: cfg.instanceHint ?? '',
    dshSockPath: cfg.dshSockPath ?? Sockets.dsh,
    heartbeatIntervalMs: cfg.heartbeatIntervalMs ?? 5000,
    blockOnFailure: cfg.blockOnFailure ?? true,
    initialRetryMs: cfg.initialRetryMs ?? 1000,
    maxRetryMs: cfg.maxRetryMs ?? 30000,
  };
  return new BootstrapCore(required);
}

// 真实 Cordis 插件入口 (name/inject/apply), DSH Loader 加载包 main 时识别
export * from './cordis.js';
