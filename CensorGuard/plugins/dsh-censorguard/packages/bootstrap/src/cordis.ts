// Bootstrap 的真实 Cordis 插件入口
//
// DSH Loader 加载包 main 时识别这里的 name/inject/apply 导出:
//   - 无 inject: 不依赖任何 DSH 服务, 整个 Profile 里最先激活, 尽早 attach
//   - apply: attach_self 成功后 ctx.provide('censorguardReady', core);
//     关键入口 (webserver/api-gateway/...) 通过 cordis.patch.yml 里的
//     inject: [censorguardReady] 等待本服务出现 (依赖闸门)
//
// ctx 用最小结构化接口声明 (运行时是 DSH vendored @deepseek-ai/cordis),
// 不在本包编译期引入 cordis 依赖。

import { createBootstrap, type BootstrapConfig, type BootstrapCore } from './index.js';

// CordisContext DSH cordis Context 的最小接口 (只用 effect/provide)
interface CordisContext {
  /** 注册随插件卸载自动回收的副作用, fn 返回 disposer */
  effect(fn: () => () => void): void;
  /** 提供服务实例, inject 该名字的 entry 会等到它出现 */
  provide(name: string, value: unknown): void;
}

export const name = 'censorguard-bootstrap';

// 不注入任何服务: 本插件必须是整个 Profile 里最早激活的一批
export const inject: string[] = [];

// Config 类型即 Profile patch 里 config 字段的形状 (cordis 不做 schema 校验时原样透传)
export type Config = BootstrapConfig;

/** apply Cordis 插件体: attach → 提供 censorguardReady → 注册 dispose */
export async function apply(ctx: CordisContext, config?: BootstrapConfig): Promise<void> {
  const cfg: BootstrapConfig = config ?? { policyGroup: 'censorguard-dsh-default' };
  const core: BootstrapCore = createBootstrap(cfg);
  // HMR/卸载只断开连接, 不调 untrack; 域由 daemon 按根进程退出回收
  ctx.effect(() => () => core.dispose());

  try {
    const result = await core.start();
    console.log(
      `[censorguard-bootstrap] attach 成功: domain=${result.domain} ` +
        `group=${result.group} version=${result.version} boot=${result.daemonBootId}`,
    );
  } catch (e) {
    // 失败策略: blockOnFailure=true (默认) 时抛出, 本插件激活失败,
    // censorguardReady 永不提供, webserver/API/Agent 不进入 Ready (fail-closed)
    if (cfg.blockOnFailure ?? true) {
      throw e;
    }
    // 非阻塞模式: degraded 态也提供服务, Host/UI 展示保护丢失, 后台重试
    console.error('[censorguard-bootstrap] attach 失败 (degraded, 后台重试):', e);
  }

  ctx.provide('censorguardReady', core);
}
