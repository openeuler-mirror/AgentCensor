// Client 浏览器半 Cordis 插件入口
//
// DSH client bundle 契约 (packages/client/modules 的 ModuleLoader):
// 本模块的导出即插件对象 (name/inject/apply); react / @deepseek-ai/cordis
// 等 specifier 由平台模块表解析, 构建时保持 external。
//
// 注册两个 slot:
//   - settings.section/security (安全策略设置页)
//   - conversation.view/security-audit order=20 (审计会话页签,
//     位于 chat(0)/trajectory(10) 右侧; 不占用 details slot)

import type { ClientConnection } from './rpc.js';
import { SecurityController, AuditController } from './controllers.js';
import { SecuritySection } from './SecuritySection.js';
import { AuditView } from './AuditView.js';

// ClientContext DSH 浏览器 cordis Context 的最小接口
interface ClientContext {
  slots: {
    /** 等 slot 声明出现后执行注册回调 (返回 disposer) */
    inject(key: string, fn: () => unknown): void;
    /** 注册一个 slot 条目: options + React 组件 */
    register(options: Record<string, unknown>, component: unknown): () => void;
  };
  connection: ClientConnection;
}

export const name = 'censorguard-ui';

// slots: 注册点; connection: RPC 调用点。声明方包 (dsh-client-ui-settings /
// dsh-client-ui-conversation) 由 package.json 的 dsh.client.inject 表达
export const inject = ['slots', 'connection'];

export function apply(ctx: ClientContext): void {
  // 控制器在 apply 层创建 (可以碰 ctx); 组件只经 inject share 拿实例
  const security = new SecurityController(ctx.connection);
  const audit = new AuditController(ctx.connection);

  // 设置页 id=security label=安全策略
  ctx.slots.inject('settings.section', () =>
    ctx.slots.register(
      {
        name: 'settings.section',
        id: 'security',
        order: 100,
        label: '安全策略',
        inject: () => ({ controller: security }),
      },
      SecuritySection,
    ),
  );

  // 会话页签 id=security-audit order=20 label=安全拦截审计
  ctx.slots.inject('conversation.view', () =>
    ctx.slots.register(
      {
        name: 'conversation.view',
        id: 'security-audit',
        order: 20,
        label: '安全拦截审计',
        inject: () => ({ controller: audit }),
      },
      AuditView,
    ),
  );
}
