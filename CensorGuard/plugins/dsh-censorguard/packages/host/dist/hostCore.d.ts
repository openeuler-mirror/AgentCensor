import { GrpcClient, type StatusReply, type GetPolicyReply, type ValidatePolicyReply, type ApplyPolicyReply, type SetSwitchesReply, type SwitchUpdate } from './grpc.js';
export type Authority = 'loopback' | 'remote';
export declare class HostError extends Error {
    readonly kind: 'forbidden' | 'version_conflict' | 'grpc_error' | 'bad_request';
    readonly code: number;
    constructor(kind: 'forbidden' | 'version_conflict' | 'grpc_error' | 'bad_request', message: string, code?: number);
}
export interface AdminPolicyRequest {
    group: string;
    policyYaml: string;
    expectedVersion?: number;
    dryRun?: boolean;
}
export declare class HostCore {
    private readonly grpc;
    private readonly boundGroup;
    private localDomainId;
    constructor(grpc: GrpcClient, boundGroup: string);
    setDomainId(id: number): void;
    isLoopback(clientIp: string): boolean;
    /** readStatus 保护状态 + 全局状态 */
    readStatus(): Promise<StatusReply>;
    /** readPolicy 读取当前策略组 YAML (编辑器加载用) */
    readPolicy(group: string): Promise<GetPolicyReply>;
    /** validatePolicy 校验策略 (dryRun 等价) */
    validatePolicy(req: AdminPolicyRequest, clientIp: string): Promise<ValidatePolicyReply>;
    /** applyPolicy 保存并重新加载 */
    applyPolicy(req: AdminPolicyRequest, clientIp: string): Promise<ApplyPolicyReply>;
    /** setSwitches 运行时开关切换 (loopback only; 远程浏览器 403) */
    setSwitches(update: SwitchUpdate, clientIp: string): Promise<SetSwitchesReply>;
    dispose(): void;
    private assertEditableGroup;
    private assertLoopback;
    private buildContext;
    private buildAdminContext;
}
//# sourceMappingURL=hostCore.d.ts.map