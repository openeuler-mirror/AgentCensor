// HostCore: 两条 RPC 通道分发 + Authority 检查 + Origin 标记
//
// RPC 调用经 cordis.ts 的路由进入, Authority 由调用方传入; DSH Connection
// trust fence 已在 handler 前拒绝远程浏览器的 admin 写请求, 这里的
// assertLoopback 是第二层兜底。apply 三道闸:
//   assertLoopback → assertEditableGroup → 乐观锁版本检查 (409)

import {
  GrpcClient,
  type StatusReply,
  type GetPolicyReply,
  type ValidatePolicyReply,
  type ApplyPolicyReply,
  type SetSwitchesReply,
  type SwitchUpdate,
  type RequestContext,
  RequestOrigin,
} from './grpc.js';

// Authority 来源
// - trusted-host: DSH 进程本机内任何 caller 都可信 (本机 WebUI / 本机插件)
// - loopback: 仅 loopback 网络来源可信 (远程浏览器不可调 admin)
export type Authority = 'loopback' | 'remote';

// HostError Host 抛出的错误 (gRPC code 映射: 403 → PermissionDenied)
export class HostError extends Error {
  constructor(
    public readonly kind: 'forbidden' | 'version_conflict' | 'grpc_error' | 'bad_request',
    message: string,
    public readonly code: number = 400,
  ) {
    super(message);
  }
}

// AdminPolicyRequest admin 路由请求体 (Client → Host)
// expectedVersion 用于乐观锁: 与服务端当前版本不一致则 version_conflict
export interface AdminPolicyRequest {
  group: string;
  policyYaml: string;
  expectedVersion?: number;
  dryRun?: boolean;
}

// HostCore DSH Host 插件核心
export class HostCore {
  // 由 Bootstrap attach 回执回填的当前 Domain (用于 RequestContext.domainId)
  private localDomainId: number = 0;

  constructor(
    private readonly grpc: GrpcClient,
    // 受信策略组 (来自 dsh.sock attach_self 回执); 限 WebUI 只编辑本组
    // 不允许在页面上编辑 __base__ 或任意其他组名
    private readonly boundGroup: string,
  ) {}

  setDomainId(id: number): void {
    this.localDomainId = id;
  }

  // ============ Authority ============
  // isLoopback 判断 client IP 是否为 loopback
  isLoopback(clientIp: string): boolean {
    if (!clientIp) return false;
    const ip = clientIp.split(':')[0] || clientIp; // 去掉端口
    return ip === '127.0.0.1' || ip === '::1' || ip === 'localhost';
  }

  // ============ /censorguard-read 路由 (trusted-host, 任何 caller 都可读) ============

  /** readStatus 保护状态 + 全局状态 */
  async readStatus(): Promise<StatusReply> {
    return this.grpc.status(this.buildContext());
  }

  /** readPolicy 读取当前策略组 YAML (编辑器加载用) */
  // 限只能读 boundGroup, 不允许读 __base__ 或其他组
  async readPolicy(group: string): Promise<GetPolicyReply> {
    this.assertEditableGroup(group);
    return this.grpc.getPolicy(group, this.buildContext());
  }

  // ============ /censorguard-admin 路由 (loopback only) ============

  /** validatePolicy 校验策略 (dryRun 等价) */
  // 远程浏览器调用得到 403
  async validatePolicy(
    req: AdminPolicyRequest,
    clientIp: string,
  ): Promise<ValidatePolicyReply> {
    this.assertLoopback(clientIp);
    this.assertEditableGroup(req.group);
    return this.grpc.validatePolicy(req.group, req.policyYaml, this.buildAdminContext());
  }

  /** applyPolicy 保存并重新加载 */
  // 流程: 检查 authority → 检查组名 → 检查 expectedVersion → grpc.ApplyPolicy
  // 任何失败保留旧策略 (gRPC ApplyPolicy 本身是原子, 这里只是前置检查)
  async applyPolicy(
    req: AdminPolicyRequest,
    clientIp: string,
  ): Promise<ApplyPolicyReply> {
    this.assertLoopback(clientIp);
    this.assertEditableGroup(req.group);

    // 1. 版本冲突检查 (乐观锁): 不传 expectedVersion 跳过
    if (req.expectedVersion != null) {
      const current = await this.grpc.getPolicy(req.group, this.buildContext());
      if (current.version !== req.expectedVersion) {
        throw new HostError(
          'version_conflict',
          `版本冲突: 期望 v${req.expectedVersion}, 服务端当前 v${current.version}, 请重新读取`,
          409,
        );
      }
    }

    // 2. 先 Validate (Validate 成功后才 Apply)
    const validate = await this.grpc.validatePolicy(
      req.group,
      req.policyYaml,
      this.buildAdminContext(),
    );
    if (!validate.ok) {
      throw new HostError('bad_request', `策略校验失败: ${validate.errors.join('; ')}`, 422);
    }

    // 3. Apply (origin=DSH_WEB_UI 标记, 透传到 daemon 审计)
    return this.grpc.applyPolicy(req.group, req.policyYaml, false, this.buildAdminContext());
  }

  /** setSwitches 运行时开关切换 (loopback only; 远程浏览器 403) */
  async setSwitches(update: SwitchUpdate, clientIp: string): Promise<SetSwitchesReply> {
    this.assertLoopback(clientIp);
    return this.grpc.setSwitches(update, this.buildAdminContext());
  }

  dispose(): void {
    this.grpc.dispose();
  }

  // ============ 内部辅助 ============

  // 仅允许编辑当前 DSH 绑定的策略组 (不允许编辑 __base__ 或其他组)
  private assertEditableGroup(group: string): void {
    if (!group || group === '__base__') {
      throw new HostError('bad_request', '不允许编辑 __base__ 或空组名', 422);
    }
    if (group !== this.boundGroup) {
      throw new HostError(
        'forbidden',
        `不允许编辑非当前绑定组: 请求 ${group}, 当前绑定 ${this.boundGroup}`,
        403,
      );
    }
  }

  // 远程浏览器写接口 403
  private assertLoopback(clientIp: string): void {
    if (!this.isLoopback(clientIp)) {
      throw new HostError(
        'forbidden',
        `远程浏览器不可调用 admin 写接口 (client=${clientIp})`,
        403,
      );
    }
  }

  // /censorguard-read 用 context: 不带 origin (只读, 不写审计)
  private buildContext(): RequestContext {
    return {
      dshInstanceId: undefined,
      domainId: this.localDomainId || undefined,
    };
  }

  // /censorguard-admin 用 context: origin=DSH_WEB_UI
  private buildAdminContext(): RequestContext {
    return {
      origin: RequestOrigin.DSH_WEB_UI,
      dshInstanceId: undefined,
      domainId: this.localDomainId || undefined,
    };
  }
}
