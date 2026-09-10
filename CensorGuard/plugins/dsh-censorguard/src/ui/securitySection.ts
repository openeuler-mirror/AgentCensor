// 安全策略设置页定义: 真实 DSH 集成时经 ctx.slots.inject('settings.section', ...)
// 注册; 这里提供可独立测试的 SECTION 定义和视图结构

// SettingsSection DSH Client 注册的 settings.section 结构
// 真实 DSH Cordis SDK 类型在 DSH 仓库集成时填, 这里只定义契约
export interface SettingsSection {
  id: string;                   // 'security'
  label: string;                 // '安全策略'
  scope: 'global' | 'session';
  order: number;                  // 100 (靠后, 不破坏既有 Settings)
}

export const SECURITY_SECTION: SettingsSection = {
  id: 'security',
  label: '安全策略',
  scope: 'global',
  order: 100,
};

// SectionView 4 区结构 (保护状态 / 策略编辑器 / 操作按钮 / 限制提示)
export interface SectionView {
  status: ProtectionStatusView;
  editor: PolicyEditorView;
  actions: ActionButtonsView;
  notice: string;                     // 当前限制提示
}

// ProtectionStatusView 保护状态视图
export interface ProtectionStatusView {
  daemonConnected: boolean;
  hooksHealthy: boolean;
  domainName: string;
  domainId: number;
  boundGroup: string;
  policyVersion: number;
  ruleVersion: number;
  reloadGen: number;
  attachAt: number;             // ms timestamp
  daemonBootId: string;
  auditStreamConnected: boolean;
  auditDroppedCount: number;
}

// PolicyEditorView 编辑器视图
export interface PolicyEditorView {
  currentYaml: string;           // 服务端当前生效的 YAML
  draftYaml: string;             // 用户正在编辑的 YAML
  dirty: boolean;                // draft !== current
  parseError: string | null;      // YAML 语法错误定位
  ruleDiagnostics: string[];       // 统一规则语法诊断
  diff: PolicyDiff;                // 与服务端版本的文本差异
  maxDocBytes: number;             // 最大文档大小限制
}

// ActionButtonsView 操作按钮
// 远程浏览器 (authority !== loopback) 时 actions 禁用
export interface ActionButtonsView {
  canApply: boolean;             // 远程浏览器为 false
  buttons: {
    reload: boolean;              // [重新读取]
    validate: boolean;            // [校验策略]
    diff: boolean;                 // [查看差异]
    apply: boolean;                 // [保存并重新加载]
  };
}

// PolicyDiff 文本差异 (按行)
export interface PolicyDiff {
  added: string[];
  removed: string[];
  unchanged: number;
}

// 当前限制提示 (固定文案)
export const SECTION_NOTICE =
  '策略热更新只保证后续系统调用使用新策略；已有文件描述符和已建立连接可能继续存在；' +
  'gRPC/事件流断线会产生审计缺口；第一阶段的 UI 来源分类不是强真人证明。';

// 编辑器最大文档大小限制
export const MAX_POLICY_DOC_BYTES = 256 * 1024; // 256 KiB
