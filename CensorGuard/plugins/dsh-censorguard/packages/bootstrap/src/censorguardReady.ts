// censorguardReady Service 定义
// Bootstrap 提供, webserver/API/Agent 通过 inject 等待。
// ctx 类型用最小结构化接口声明 (运行时是 DSH vendored @deepseek-ai/cordis),
// 本包编译期不引入 cordis 依赖。

import type { ProtectionState } from '@censorguard/dsh-runtime';

// CensorguardReady Service 接口: consumer 经 inject 拿到实例,
// 查询保护状态 / 订阅变化 / 等待就绪 / 触发重 attach
export interface CensorguardReady {
  /** 当前保护状态 (attaching/protected/degraded) */
  readonly state: ProtectionState;
  /** 订阅状态变化 */
  subscribe(fn: (s: ProtectionState) => void): () => void;
  /** 等待进入 protected 态 (Bootstrap 内部用; consumer 应通过 inject 等待) */
  waitForReady(timeoutMs?: number): Promise<void>;
  /** 触发重新 attach (daemon 重启后) */
  reattach(): Promise<void>;
}

// Service key (Cordis 约定: provide/inject 的名字)
export const CENSORGUARD_READY_KEY = 'censorguardReady';
