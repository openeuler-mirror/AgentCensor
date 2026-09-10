import { type PolicyDiff } from './securitySection.js';
export interface PolicyEditorState {
    currentYaml: string;
    draftYaml: string;
    currentVersion: number;
    dirty: boolean;
    parseError: string | null;
    ruleDiagnostics: string[];
    overDocLimit: boolean;
}
export declare class PolicyEditor {
    private state;
    constructor();
    get(): PolicyEditorState;
    loadCurrent(text: string, version: number): void;
    setDraft(text: string): void;
    setRuleDiagnostics(errors: readonly string[]): void;
    detectVersionConflict(serverVersion: number): boolean;
    computeDiff(): PolicyDiff;
    canSave(): boolean;
    staticYamlSyntaxCheck(plain: string): string | null;
    buildApplyRequest(): {
        policyYaml: string;
        expectedVersion: number;
    } | null;
}
export declare function computeLineDiff(oldText: string, newText: string): PolicyDiff;
//# sourceMappingURL=policyEditor.d.ts.map