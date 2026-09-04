// PolicyEditor 客户端编辑器逻辑 (MOCK)
//
// 规则解析/打包后期要换成另一种格式, 本文件只做状态机占位:
//   - 保留 dirty/version/诊断/大小上限/乐观锁 等视图契约 (SecuritySection 依赖)
//   - 规则文本不做结构化解析: 不拆行、不校验 file/exec/net 头、不打包
//     rules: YAML 片段, buildApplyRequest 原样透传草稿
// TODO(rules): 新规则格式落地后, 替换 staticYamlSyntaxCheck / buildApplyRequest
// 的透传实现, 并恢复编辑器侧的行级诊断。
import { MAX_POLICY_DOC_BYTES } from './securitySection.js';
// byteLength UTF-8 字节数 (浏览器半不能用 Node 的 Buffer)
const utf8 = new TextEncoder();
function byteLength(s) {
    return utf8.encode(s).length;
}
// PolicyEditor 客户端编辑器逻辑 (mock: 纯状态机, 无规则格式解析)
export class PolicyEditor {
    state;
    constructor() {
        this.state = {
            currentYaml: '',
            draftYaml: '',
            currentVersion: 0,
            dirty: false,
            parseError: null,
            ruleDiagnostics: [],
            overDocLimit: false,
        };
    }
    get() {
        return this.state;
    }
    // 从服务端加载当前策略 (由 Controller 调 Host readPolicy 后传入原文)
    loadCurrent(text, version) {
        this.state.currentYaml = text;
        this.state.draftYaml = text;
        this.state.currentVersion = version;
        this.state.dirty = false;
        this.state.parseError = null;
        this.state.ruleDiagnostics = [];
        this.state.overDocLimit = byteLength(text) > MAX_POLICY_DOC_BYTES;
    }
    // 用户修改编辑器内容
    setDraft(text) {
        this.state.draftYaml = text;
        this.state.dirty = text !== this.state.currentYaml;
        this.state.overDocLimit = byteLength(text) > MAX_POLICY_DOC_BYTES;
        this.state.parseError = null;
        this.state.ruleDiagnostics = [];
    }
    // 写入服务端 Validate 返回的规则级诊断 (有诊断时 canSave=false)
    setRuleDiagnostics(errors) {
        this.state.ruleDiagnostics = errors.slice();
    }
    // 版本冲突检测 (Host apply 报 version_conflict 时由 Controller 调)
    detectVersionConflict(serverVersion) {
        return serverVersion !== this.state.currentVersion;
    }
    // 查看差异 ([查看差异] 按钮; 通用行级 diff, 与规则格式无关)
    computeDiff() {
        return computeLineDiff(this.state.currentYaml, this.state.draftYaml);
    }
    // 是否可保存 (dirty + 无诊断 + 未超限)
    canSave() {
        return (this.state.dirty &&
            !this.state.parseError &&
            this.state.ruleDiagnostics.length === 0 &&
            !this.state.overDocLimit);
    }
    // 保存前静态语法检查 (MOCK: 只查非空; 真实规则校验在 Host/daemon 端)
    // TODO(rules): 新格式落地后恢复编辑器侧语法预检
    staticYamlSyntaxCheck(plain) {
        if (!plain.trim())
            return '策略为空';
        return null;
    }
    // 构造保存请求体 (MOCK: 草稿原样透传, 不做 rules: 打包;
    // group 不传 —— Host 路由层默认当前绑定组, Client 不需要知道组名)
    // TODO(rules): 新格式落地后在此做格式转换
    buildApplyRequest() {
        if (!this.canSave())
            return null;
        return {
            policyYaml: this.state.draftYaml,
            expectedVersion: this.state.currentVersion,
        };
    }
}
// computeLineDiff 简单行级 diff (LCS 太重, 这里用 set 比较)
export function computeLineDiff(oldText, newText) {
    const oldLines = oldText.split('\n');
    const newLines = newText.split('\n');
    const oldSet = new Set(oldLines);
    const newSet = new Set(newLines);
    const added = newLines.filter((l) => !oldSet.has(l));
    const removed = oldLines.filter((l) => !newSet.has(l));
    const unchanged = oldLines.filter((l) => newSet.has(l)).length;
    return { added, removed, unchanged };
}
//# sourceMappingURL=policyEditor.js.map