// @censorguard/dsh UI entry (node 半)
// WebUI 安全策略设置页 + YAML 编辑器 + 审计会话页签 (conversation.view/security-audit)
//
// DSH 双半结构 (deepseek-harness packages/client/ui-workspace 同款):
// node 半是 host cordis 插件体, loader 按 entry name 从包根解析到这里,
// 只需空 apply 让插件进入 host 树 (加载/生命周期跟随 host);
// 浏览器半在 ./client 导出 (lib/client.js, dsh.client manifest 发现),
// 真正的 slot 注册在 src/client/index.ts。
// 下方其余导出是 node 侧测试用的库符号, host 不消费。

/** Host 插件体: 纯 UI 插件, 浏览器半承载全部行为。 */
export function apply(): void {}

export * from './securitySection.js';
export * from './policyEditor.js';
export * from './auditTab.js';
export * from './auditView.js';
