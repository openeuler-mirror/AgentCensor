// censorguardReady Service 定义
// Bootstrap 提供, webserver/API/Agent 通过 inject 等待。
// ctx 类型用最小结构化接口声明 (运行时是 DSH vendored @deepseek-ai/cordis),
// 本包编译期不引入 cordis 依赖。
// Service key (Cordis 约定: provide/inject 的名字)
export const CENSORGUARD_READY_KEY = 'censorguardReady';
//# sourceMappingURL=censorguardReady.js.map