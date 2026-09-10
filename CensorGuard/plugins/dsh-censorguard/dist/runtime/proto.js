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
};
// AttachError attach_self 校验失败错误类型
export class AttachError extends Error {
    kind;
    constructor(kind, message) {
        super(message);
        this.kind = kind;
        this.name = 'AttachError';
    }
}
//# sourceMappingURL=proto.js.map