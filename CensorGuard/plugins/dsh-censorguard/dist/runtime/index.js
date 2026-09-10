// runtime 模块入口: dsh.sock AttachSelf 客户端 + 保护状态机 (无 DSH 依赖)
// Bootstrap 用 AttachSelf 客户端 + ProtectionState 状态机;
// Host 用 stateMachine 查询当前保护状态。
export * from './proto.js';
export * from './state.js';
export * from './attach.js';
//# sourceMappingURL=index.js.map