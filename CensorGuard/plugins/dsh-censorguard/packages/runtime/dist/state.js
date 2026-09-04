// ProtectionState 状态机
// 状态转换: attaching -> protected | degraded; protected -> degraded (心跳失败);
// degraded -> protected (重连)
// ProtectionStateMachine 状态机 + 订阅
// 状态机本身不持有 Cordis ctx, 由 Bootstrap 通过 apply/dispose 生命周期管理
export class ProtectionStateMachine {
    state = { kind: 'attaching' };
    listeners = new Set();
    get() {
        return this.state;
    }
    subscribe(fn) {
        this.listeners.add(fn);
        fn(this.state);
        return () => {
            this.listeners.delete(fn);
        };
    }
    /** 切到 protected 状态 (attach_self 成功) */
    toProtected(p) {
        const next = { kind: 'protected', ...p, attachAt: Date.now() };
        this.transition(next);
    }
    /** 切到 degraded 状态 (心跳失败 / daemon 重启 / hook 不健康) */
    toDegraded(reason) {
        const next = { kind: 'degraded', reason, since: Date.now() };
        this.transition(next);
    }
    /** 重新进入 attaching (daemon 重启后重新 attach 前) */
    toAttaching() {
        this.transition({ kind: 'attaching' });
    }
    transition(next) {
        this.state = next;
        for (const fn of this.listeners) {
            try {
                fn(next);
            }
            catch {
                // 监听器异常不影响状态机
            }
        }
    }
}
// 便捷工具: 判断当前是否处于受保护状态
export function isProtected(s) {
    return s.kind === 'protected';
}
//# sourceMappingURL=state.js.map