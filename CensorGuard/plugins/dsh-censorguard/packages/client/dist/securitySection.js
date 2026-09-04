// 安全策略设置页定义: 真实 DSH 集成时经 ctx.slots.inject('settings.section', ...)
// 注册; 这里提供可独立测试的 SECTION 定义和视图结构
export const SECURITY_SECTION = {
    id: 'security',
    label: '安全策略',
    scope: 'global',
    order: 100,
};
// 当前限制提示 (固定文案)
export const SECTION_NOTICE = '策略热更新只保证后续系统调用使用新策略；已有文件描述符和已建立连接可能继续存在；' +
    'gRPC/事件流断线会产生审计缺口；第一阶段的 UI 来源分类不是强真人证明。';
// 编辑器最大文档大小限制
export const MAX_POLICY_DOC_BYTES = 256 * 1024; // 256 KiB
//# sourceMappingURL=securitySection.js.map