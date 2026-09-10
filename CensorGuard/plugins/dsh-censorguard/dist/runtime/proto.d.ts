export declare const PROTO_VERSION = 2;
export declare const MAX_LINE_BYTES: number;
export declare const Sockets: {
    readonly ctl: "/run/censorguard/ctl.sock";
    readonly dsh: "/run/censorguard/dsh.sock";
    readonly ui: "/run/censorguard/ui.sock";
    readonly launch: "/run/censorguard/launch.sock";
    readonly events: "/run/censorguard/events.sock";
};
export type SelfServeMethod = 'attach_self' | 'status_self' | 'tree_self';
export type RpcMethod = SelfServeMethod | 'health' | 'get_capabilities' | 'get_policy' | 'validate_policy' | 'apply_policy' | 'rollback_policy' | 'evaluate_intent' | 'register_self' | 'rebind_scope' | 'close_scope' | 'list_scopes' | 'list_trees' | 'get_metrics';
export interface RpcParams {
    scope?: string;
    group?: string;
    policy_yaml?: string;
    expected_revision?: number;
    revision?: number;
    idempotency_key?: string;
    policy_group?: string;
    instance_hint?: string;
    seed?: boolean;
}
export interface RpcRequest {
    v: number;
    request_id: string;
    method: RpcMethod;
    params: RpcParams;
}
export interface DomainInfo {
    name: string;
    id: number;
    slot: number;
    group: string;
    roots: number;
    version: number;
    draining?: boolean;
}
export interface LegacyResponse {
    ok: boolean;
    error?: string;
    seeded?: number;
    domain?: DomainInfo;
    domains?: DomainInfo[];
    roots?: number[];
    tracked?: number;
    daemon_boot_id?: string;
    hooks_healthy?: boolean;
    [key: string]: unknown;
}
export interface RpcResult {
    response?: LegacyResponse;
    revision?: number;
    policy_yaml?: string;
    capabilities?: string[];
}
export interface RpcErrorBody {
    code: string;
    message: string;
    current_revision?: number;
}
export interface RpcResponse {
    v: number;
    request_id: string;
    ok: boolean;
    result?: RpcResult;
    error?: RpcErrorBody;
}
export declare class AttachError extends Error {
    readonly kind: 'connect' | 'daemon_down' | 'attach_failed' | 'hooks_unhealthy' | 'group_not_allowed' | 'version_mismatch';
    constructor(kind: 'connect' | 'daemon_down' | 'attach_failed' | 'hooks_unhealthy' | 'group_not_allowed' | 'version_mismatch', message: string);
}
//# sourceMappingURL=proto.d.ts.map