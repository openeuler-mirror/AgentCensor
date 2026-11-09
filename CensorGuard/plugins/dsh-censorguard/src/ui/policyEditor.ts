// 策略 YAML 由 Host/daemon 统一解析和校验, 避免客户端维护另一套规则语法。

import { MAX_POLICY_DOC_BYTES, type PolicyDiff } from './securitySection.js';

// byteLength UTF-8 字节数 (浏览器半不能用 Node 的 Buffer)
const utf8 = new TextEncoder();

function byteLength(s: string): number {
  return utf8.encode(s).length;
}

// PolicyEditorState 编辑器状态
export interface PolicyEditorState {
  currentYaml: string;            // 服务端当前生效文本
  draftYaml: string;              // 用户编辑中文本
  currentVersion: number;         // 服务端版本 (乐观锁用)
  dirty: boolean;                  // draft !== current
  parseError: string | null;
  ruleDiagnostics: string[];
  overDocLimit: boolean;
}

export class PolicyEditor {
  private state: PolicyEditorState;

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

  get(): PolicyEditorState {
    return this.state;
  }

  // 从服务端加载当前策略 (由 Controller 调 Host readPolicy 后传入原文)
  loadCurrent(text: string, version: number): void {
    this.state.currentYaml = text;
    this.state.draftYaml = text;
    this.state.currentVersion = version;
    this.state.dirty = false;
    this.state.parseError = null;
    this.state.ruleDiagnostics = [];
    this.state.overDocLimit = byteLength(text) > MAX_POLICY_DOC_BYTES;
  }

  // 用户修改编辑器内容
  setDraft(text: string): void {
    this.state.draftYaml = text;
    this.state.dirty = text !== this.state.currentYaml;
    this.state.overDocLimit = byteLength(text) > MAX_POLICY_DOC_BYTES;
    this.state.parseError = null;
    this.state.ruleDiagnostics = [];
  }

  // 写入服务端 Validate 返回的规则级诊断 (有诊断时 canSave=false)
  setRuleDiagnostics(errors: readonly string[]): void {
    this.state.ruleDiagnostics = errors.slice();
  }

  // 版本冲突检测 (Host apply 报 version_conflict 时由 Controller 调)
  detectVersionConflict(serverVersion: number): boolean {
    return serverVersion !== this.state.currentVersion;
  }

  // 查看差异 ([查看差异] 按钮; 通用行级 diff, 与规则格式无关)
  computeDiff(): PolicyDiff {
    return computeLineDiff(this.state.currentYaml, this.state.draftYaml);
  }

  // 是否可保存 (dirty + 无诊断 + 未超限)
  canSave(): boolean {
    return (
      this.state.dirty &&
      !this.state.parseError &&
      this.state.ruleDiagnostics.length === 0 &&
      !this.state.overDocLimit
    );
  }

  // 此处仅检查非空; YAML 语法和规则校验交给 Host/daemon。
  staticYamlSyntaxCheck(plain: string): string | null {
    if (!plain.trim()) return '策略为空';
    return null;
  }

  // group 由 Host 路由层取当前绑定组, 客户端只提交原文和乐观锁版本。
  buildApplyRequest(): { policyYaml: string; expectedVersion: number } | null {
    if (!this.canSave()) return null;
    return {
      policyYaml: this.state.draftYaml,
      expectedVersion: this.state.currentVersion,
    };
  }
}

// computeLineDiff 简单行级 diff (LCS 太重, 这里用 set 比较)
export function computeLineDiff(oldText: string, newText: string): PolicyDiff {
  const oldLines = oldText.split('\n');
  const newLines = newText.split('\n');
  const oldSet = new Set(oldLines);
  const newSet = new Set(newLines);

  const added = newLines.filter((l) => !oldSet.has(l));
  const removed = oldLines.filter((l) => !newSet.has(l));
  const unchanged = oldLines.filter((l) => newSet.has(l)).length;

  return { added, removed, unchanged };
}
