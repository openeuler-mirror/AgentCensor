export interface SettingsSection {
    id: string;
    label: string;
    scope: 'global' | 'session';
    order: number;
}
export declare const SECURITY_SECTION: SettingsSection;
export interface SectionView {
    status: ProtectionStatusView;
    editor: PolicyEditorView;
    actions: ActionButtonsView;
    notice: string;
}
export interface ProtectionStatusView {
    daemonConnected: boolean;
    hooksHealthy: boolean;
    domainName: string;
    domainId: number;
    boundGroup: string;
    policyVersion: number;
    ruleVersion: number;
    reloadGen: number;
    attachAt: number;
    daemonBootId: string;
    auditStreamConnected: boolean;
    auditDroppedCount: number;
}
export interface PolicyEditorView {
    currentYaml: string;
    draftYaml: string;
    dirty: boolean;
    parseError: string | null;
    ruleDiagnostics: string[];
    diff: PolicyDiff;
    maxDocBytes: number;
}
export interface ActionButtonsView {
    canApply: boolean;
    buttons: {
        reload: boolean;
        validate: boolean;
        diff: boolean;
        apply: boolean;
    };
}
export interface PolicyDiff {
    added: string[];
    removed: string[];
    unchanged: number;
}
export declare const SECTION_NOTICE: string;
export declare const MAX_POLICY_DOC_BYTES: number;
//# sourceMappingURL=securitySection.d.ts.map