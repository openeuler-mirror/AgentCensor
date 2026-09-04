// 安全策略设置页组件: 保护状态 / 策略编辑器 / 操作按钮 / 限制提示
// 组件不接触 ctx, 一切数据经 props.controller (SecurityController)
//
// 视觉: 对标原生 DSH WebUI 浅色风格 —— 卡片分区 + 药丸徽章 + 主/次按钮层级,
// 与审计页签 (AuditView) 共用同一套调色板。

import { useEffect, useSyncExternalStore, type CSSProperties, type FC } from 'react';
import type { SecurityController } from './controllers.js';

export interface SecuritySectionProps {
  controller: SecurityController;
}

// ---------- 调色板 (浅色, 与 AuditView 保持一致) ----------
const C = {
  surface: '#f5f6f8',
  surfaceDeep: '#ffffff',
  border: '#e5e6eb',
  borderStrong: '#d5d7de',
  text: '#1f2329',
  text2: '#41464f',
  text3: '#8f959e',
  accent: '#3370ff',
  accentDim: 'rgba(51,112,255,0.06)',
  ok: '#2ea121',
  okDim: 'rgba(52,199,36,0.12)',
  deny: '#e5484d',
  warn: '#d87800',
  warnDim: 'rgba(255,136,0,0.10)',
  mono: "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace",
  sans: "system-ui, -apple-system, 'Segoe UI', Roboto, 'PingFang SC', 'Microsoft YaHei', sans-serif",
};

const styles: Record<string, CSSProperties> = {
  root: {
    display: 'flex',
    flexDirection: 'column',
    gap: 14,
    padding: 16,
    overflow: 'auto',
    fontFamily: C.sans,
    color: C.text,
    fontSize: 13,
  },
  card: {
    background: C.surface,
    border: `1px solid ${C.border}`,
    borderRadius: 10,
    padding: 16,
  },
  cardTitle: {
    fontSize: 13,
    fontWeight: 600,
    color: C.text,
    marginBottom: 12,
    letterSpacing: '0.02em',
  },

  // ---- 保护状态: 标签/值 两列网格 ----
  statusGrid: { display: 'grid', gridTemplateColumns: '120px 1fr', rowGap: 8, columnGap: 12, alignItems: 'center' },
  statusLabel: { color: C.text3, fontSize: 12 },
  statusValue: { fontFamily: C.mono, fontSize: 12, color: C.text, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' },
  badge: {
    display: 'inline-block',
    padding: '1px 10px',
    borderRadius: 999,
    fontSize: 11,
    fontWeight: 600,
    fontFamily: C.sans,
    letterSpacing: '0.03em',
  },
  badgeOk: { color: C.ok, background: C.okDim },
  badgeBad: { color: C.deny, background: 'rgba(245,74,69,0.10)' },
  loading: { color: C.text3, fontSize: 12 },

  // ---- 策略编辑器 ----
  editorHead: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 10 },
  dirtyBadge: {
    display: 'inline-block',
    padding: '1px 10px',
    borderRadius: 999,
    fontSize: 11,
    fontWeight: 600,
  },
  textarea: {
    width: '100%',
    minHeight: 240,
    boxSizing: 'border-box',
    fontFamily: C.mono,
    fontSize: 12,
    lineHeight: 1.6,
    background: C.surfaceDeep,
    color: C.text,
    border: `1px solid ${C.borderStrong}`,
    borderRadius: 8,
    padding: 10,
    outline: 'none',
    resize: 'vertical',
  },
  diag: { color: C.deny, fontSize: 12, fontFamily: C.mono, lineHeight: 1.6 },

  // ---- 差异 ----
  diffBox: {
    marginTop: 10,
    background: C.surfaceDeep,
    border: `1px solid ${C.border}`,
    borderRadius: 8,
    padding: 10,
    fontFamily: C.mono,
    fontSize: 12,
    lineHeight: 1.6,
  },
  diffSummary: { color: C.text2, marginBottom: 4 },
  diffAdd: { color: C.ok },
  diffDel: { color: C.deny },

  // ---- 操作按钮 ----
  actions: { display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' },
  btnPrimary: {
    background: C.accent,
    color: '#fff',
    border: 'none',
    borderRadius: 8,
    padding: '7px 16px',
    fontSize: 13,
    fontFamily: C.sans,
    fontWeight: 500,
    cursor: 'pointer',
  },
  btnGhost: {
    background: 'transparent',
    color: C.text2,
    border: `1px solid ${C.borderStrong}`,
    borderRadius: 8,
    padding: '6px 14px',
    fontSize: 13,
    fontFamily: C.sans,
    cursor: 'pointer',
  },
  disabled: { opacity: 0.45, cursor: 'not-allowed' },
  message: { fontSize: 12, marginLeft: 'auto' },
  msgOk: { color: C.ok },
  msgErr: { color: C.deny },
  readonlyNotice: { width: '100%', color: C.warn, fontSize: 12 },

  // ---- 限制提示 ----
  notice: {
    color: C.text2,
    fontSize: 12,
    lineHeight: 1.8,
    background: C.accentDim,
    borderLeft: `3px solid ${C.accent}`,
    borderRadius: 6,
    padding: '10px 14px',
  },
};

// msgStyle 操作反馈着色: 失败词红, 其余绿
function msgStyle(message: string): CSSProperties {
  const bad = /失败|错误|冲突|无权|不可/.test(message);
  return { ...styles.message, ...(bad ? styles.msgErr : styles.msgOk) };
}

// StatusBadge 布尔状态徽章
const StatusBadge: FC<{ ok: boolean; okText: string; badText: string }> = ({ ok, okText, badText }) => (
  <span style={{ ...styles.badge, ...(ok ? styles.badgeOk : styles.badgeBad) }}>
    {ok ? okText : badText}
  </span>
);

export const SecuritySection: FC<SecuritySectionProps> = (props) => {
  const c = props.controller;
  const s = useSyncExternalStore(c.subscribe, c.getSnapshot);

  useEffect(() => {
    void c.refresh();
  }, [c]);

  const st = s.status;
  return (
    <div style={styles.root}>
      {/* 保护状态 */}
      <section style={styles.card}>
        <div style={styles.cardTitle}>保护状态</div>
        {st ? (
          <div style={styles.statusGrid}>
            <span style={styles.statusLabel}>daemon 连接</span>
            <StatusBadge ok={st.daemonConnected} okText="已连接" badText="断开" />
            <span style={styles.statusLabel}>Hook 健康</span>
            <StatusBadge ok={st.hooksHealthy} okText="正常" badText="异常" />
            <span style={styles.statusLabel}>当前 Domain</span>
            <span style={styles.statusValue} title={st.domainName}>
              {st.domainName || '(未归属)'} {st.domainId ? `(id=${st.domainId})` : ''}
            </span>
            <span style={styles.statusLabel}>策略组</span>
            <span style={styles.statusValue} title={st.boundGroup}>
              {st.boundGroup}
            </span>
            <span style={styles.statusLabel}>策略版本</span>
            <span style={styles.statusValue}>
              v{st.policyVersion} · reloadGen {st.reloadGen}
            </span>
            <span style={styles.statusLabel}>daemon boot</span>
            <span style={styles.statusValue} title={st.daemonBootId}>
              {st.daemonBootId}
            </span>
          </div>
        ) : (
          <div style={styles.loading}>加载中…</div>
        )}
      </section>

      {/* 策略编辑器 */}
      <section style={styles.card}>
        <div style={styles.editorHead}>
          <div style={{ ...styles.cardTitle, marginBottom: 0 }}>策略编辑器</div>
          <span
            style={{
              ...styles.dirtyBadge,
              ...(s.editor.dirty
                ? { color: C.warn, background: C.warnDim }
                : { color: C.ok, background: C.okDim }),
            }}
          >
            {s.editor.dirty ? '未保存' : '已同步'}
          </span>
        </div>
        <textarea
          style={styles.textarea}
          value={s.editor.draftYaml}
          disabled={!s.canOperate}
          onChange={(e: { target: { value: string } }) => c.setDraft(e.target.value)}
        />
        {s.editor.overDocLimit ? <div style={styles.diag}>文档超过大小限制, 不可保存</div> : null}
        {s.editor.parseError ? <div style={styles.diag}>语法错误: {s.editor.parseError}</div> : null}
        {s.editor.ruleDiagnostics.map((d: string) => (
          <div key={d} style={styles.diag}>
            {d}
          </div>
        ))}
        {s.diff ? (
          <div style={styles.diffBox}>
            <div style={styles.diffSummary}>
              差异: +{s.diff.added.length} / -{s.diff.removed.length} (不变 {s.diff.unchanged} 行)
            </div>
            {s.diff.added.map((l: string) => (
              <div key={`+${l}`} style={styles.diffAdd}>
                + {l}
              </div>
            ))}
            {s.diff.removed.map((l: string) => (
              <div key={`-${l}`} style={styles.diffDel}>
                - {l}
              </div>
            ))}
          </div>
        ) : null}
      </section>

      {/* 操作按钮 (远程浏览器禁用) */}
      <section style={{ ...styles.card, ...styles.actions }}>
        <button style={{ ...styles.btnGhost, ...(s.busy ? styles.disabled : undefined) }} disabled={s.busy} onClick={() => void c.refresh()}>
          重新读取
        </button>
        <button
          style={{
            ...styles.btnGhost,
            ...(s.busy || !s.canOperate ? styles.disabled : undefined),
          }}
          disabled={s.busy || !s.canOperate}
          onClick={() => void c.validate()}
        >
          校验策略
        </button>
        <button
          style={{ ...styles.btnGhost, ...(s.busy || !s.editor.dirty ? styles.disabled : undefined) }}
          disabled={s.busy || !s.editor.dirty}
          onClick={() => c.showDiff()}
        >
          查看差异
        </button>
        <button
          style={{
            ...styles.btnPrimary,
            ...(s.busy || !s.canOperate || !s.editor.dirty ? styles.disabled : undefined),
          }}
          disabled={s.busy || !s.canOperate || !s.editor.dirty}
          onClick={() => void c.apply()}
        >
          保存并重新加载
        </button>
        {s.message ? <span style={msgStyle(s.message)}>{s.message}</span> : null}
        {!s.canOperate ? <span style={styles.readonlyNotice}>远程访问只读, 写接口不可用</span> : null}
      </section>

      {/* 当前限制提示 (固定文案) */}
      <section style={styles.card}>
        <div style={styles.notice}>{s.notice}</div>
      </section>
    </div>
  );
};
