// 协议层: 与 crates/censorguard-common/src/protocol.rs 严格对齐的
// JSON-lines over Unix socket (v2 信封: request_id + method + params)。
// Bootstrap 经 dsh.sock 调 attach_self / status_self / tree_self,
// daemon 用 SO_PEERCRED 取真实 pid, 客户端无法伪造。

// 协议版本 (protocol.rs VERSION)
export const PROTO_VERSION = 2;

// 单行最大字节 (protocol.rs MAX_LINE_BYTES)
export const MAX_LINE_BYTES = 64 * 1024;

// Socket 路径 (protocol.rs DEFAULT_*_SOCKET)
export const Sockets = {
  ctl: '/run/censorguard/ctl.sock',
  dsh: '/run/censorguard/dsh.sock',
  ui: '/run/censorguard/ui.sock',
  launch: '/run/censorguard/launch.sock',
  events: '/run/censorguard/events.sock',
} as const;

// dsh.sock 角色 ACL 允许的自助 op (server.rs role_allows)
export type SelfServeMethod = 'attach_self' | 'status_self' | 'tree_self';

export type RpcMethod =
  | SelfServeMethod
  | 'health'
  | 'get_capabilities'
  | 'get_policy'
  | 'validate_policy'
  | 'apply_policy'
  | 'rollback_policy'
  | 'evaluate_intent'
  | 'register_self'
  | 'rebind_scope'
  | 'close_scope'
  | 'list_scopes'
  | 'list_trees'
  | 'get_metrics';

// RpcParams 控制面请求参数 (protocol.rs RpcParams; attach_self 用
// policy_group/instance_hint/seed, 显式不传 pid —— daemon 以 SO_PEERCRED 为准)
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

// RpcRequest v2 请求信封 (protocol.rs RpcRequest)
export interface RpcRequest {
  v: number;
  request_id: string;
  method: RpcMethod;
  params: RpcParams;
}

// DomainInfo 域信息 (protocol.rs DomainInfo)
export interface DomainInfo {
  name: string;
  id: number;
  slot: number;
  group: string;
  roots: number;
  version: number;
  draining?: boolean;
}

// LegacyResponse 控制面响应体 (protocol.rs Response; attach/status_self
// 回执带 daemon_boot_id 与 hooks_healthy)
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

// RpcResult v2 成功结果 (protocol.rs RpcResult)
export interface RpcResult {
  response?: LegacyResponse;
  revision?: number;
  policy_yaml?: string;
  capabilities?: string[];
}

// RpcErrorBody v2 错误体 (protocol.rs RpcError)
export interface RpcErrorBody {
  code: string;
  message: string;
  current_revision?: number;
}

// RpcResponse v2 响应信封 (protocol.rs RpcResponse)
export interface RpcResponse {
  v: number;
  request_id: string;
  ok: boolean;
  result?: RpcResult;
  error?: RpcErrorBody;
}

// AttachError attach_self 校验失败错误类型
export class AttachError extends Error {
  constructor(
    public readonly kind:
      | 'connect'
      | 'daemon_down'
      | 'attach_failed'
      | 'hooks_unhealthy'
      | 'group_not_allowed'
      | 'version_mismatch',
    message: string,
  ) {
    super(message);
    this.name = 'AttachError';
  }
}
