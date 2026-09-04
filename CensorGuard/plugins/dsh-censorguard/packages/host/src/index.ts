// @censorguard/dsh-host 入口
// gRPC 桥 (grpc) + 两条 RPC 通道分发 (hostCore) + 审计事件流有界缓存
// (auditStream); cordis.js 是真实 Cordis 插件入口 (name/inject/apply),
// DSH Loader 加载包 main 时识别; 其余导出保持可独立测试

export * from './grpc.js';
export * from './hostCore.js';
export * from './auditStream.js';
export * from './cordis.js';
