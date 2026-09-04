// 审计会话页签组件: 状态条 + 过滤 + 事件列表 + 详情 JSON
// 组件不接触 ctx, 一切数据经 props.controller (AuditController)
//
// 视觉: 对标原生 DSH WebUI 浅色风格 —— 分栏表头 + 列对齐网格 + 药丸徽章;
// 过滤区为"草稿态", 点 [查找] (或回车) 才应用到控制器, 避免边打字边过滤。

import {
  useEffect,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type FC,
} from 'react';
import type { AuditController } from './controllers.js';
import {
  eventDetailJson,
  AUDIT_DISCONNECTED_HINT,
  AUDIT_EMPTY_TEXT,
  type AuditRow,
} from '../auditView.js';
import type { RpcAuditEvent } from '../auditTab.js';

export interface AuditViewProps {
  controller: AuditController;
}

// ---------- 调色板 (浅色, 与原生 DSH WebUI 白色风格对齐) ----------
const C = {
  bg: 'transparent',
  surface: '#f5f6f8',
  surfaceDeep: '#ffffff',
  border: '#e5e6eb',
  borderStrong: '#d5d7de',
  text: '#1f2329',
  text2: '#41464f',
  text3: '#8f959e',
  accent: '#3370ff',
  accentDim: 'rgba(51,112,255,0.08)',
  ok: '#2ea121',
  okDim: 'rgba(52,199,36,0.12)',
  deny: '#e5484d',
  denyDim: 'rgba(245,74,69,0.10)',
  warn: '#d87800',
  warnDim: 'rgba(255,136,0,0.10)',
  mono: "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace",
  sans: "system-ui, -apple-system, 'Segoe UI', Roboto, 'PingFang SC', 'Microsoft YaHei', sans-serif",
};

// 列模板: 时间/结果/类型/Domain/进程/目标/参数/版本
// 表头与数据行共用, 保证列对齐; minmax+0fr 让超出部分按比例伸缩
const GRID_COLS =
  '78px 66px 122px minmax(84px, 0.8fr) 148px minmax(120px, 1.1fr) minmax(120px, 1.3fr) 64px';

// 通用单元格: 超长截断 + title 悬浮全文
const cell = (extra?: CSSProperties): CSSProperties => ({
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
  minWidth: 0,
  ...extra,
});

const styles: Record<string, CSSProperties> = {
  root: { display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden', background: C.bg, fontFamily: C.sans, color: C.text },

  // ---- 顶部状态条: 药丸式指标 ----
  bar: { display: 'flex', gap: 6, padding: '8px 12px', flexWrap: 'wrap', alignItems: 'center' },
  chip: {
    display: 'inline-flex',
    alignItems: 'center',
    gap: 5,
    padding: '2px 10px',
    borderRadius: 999,
    background: C.surface,
    border: `1px solid ${C.border}`,
    color: C.text2,
    fontSize: 11,
    fontFamily: C.mono,
    whiteSpace: 'nowrap',
  },
  dotLive: { color: C.ok, fontSize: 10 },
  dotBad: { color: C.warn, fontSize: 10 },
  barHint: { fontSize: 11, color: C.warn, padding: '0 12px 6px' },

  // ---- 过滤区: 草稿态, [查找] 应用 ----
  filters: {
    display: 'flex',
    gap: 8,
    padding: '8px 12px',
    borderTop: `1px solid ${C.border}`,
    borderBottom: `1px solid ${C.border}`,
    background: C.surface,
    flexWrap: 'wrap',
    alignItems: 'center',
  },
  input: {
    background: C.surfaceDeep,
    border: `1px solid ${C.borderStrong}`,
    borderRadius: 6,
    padding: '5px 10px',
    color: C.text,
    fontSize: 12,
    fontFamily: C.mono,
    outline: 'none',
  },
  btnPrimary: {
    background: C.accent,
    color: '#fff',
    border: 'none',
    borderRadius: 6,
    padding: '5px 16px',
    fontSize: 12,
    fontFamily: C.sans,
    fontWeight: 500,
    cursor: 'pointer',
  },
  btnGhost: {
    background: 'transparent',
    color: C.text2,
    border: `1px solid ${C.borderStrong}`,
    borderRadius: 6,
    padding: '5px 14px',
    fontSize: 12,
    fontFamily: C.sans,
    cursor: 'pointer',
  },

  // ---- 事件表: 表头 + 网格行 ----
  list: { flex: 1, overflow: 'auto', minHeight: 0 },
  head: {
    display: 'grid',
    gridTemplateColumns: GRID_COLS,
    gap: '0 10px',
    position: 'sticky',
    top: 0,
    zIndex: 1,
    background: C.surface,
    borderBottom: `1px solid ${C.border}`,
    padding: '6px 12px',
    color: C.text3,
    fontSize: 11,
    fontWeight: 600,
    letterSpacing: '0.04em',
    userSelect: 'none',
  },
  row: {
    display: 'grid',
    gridTemplateColumns: GRID_COLS,
    gap: '0 10px',
    alignItems: 'center',
    padding: '5px 12px',
    cursor: 'pointer',
    fontSize: 12,
    fontFamily: C.mono,
    borderBottom: `1px solid rgba(229,230,235,0.9)`,
  },
  gapRow: {
    padding: '4px 12px',
    color: C.warn,
    background: C.warnDim,
    fontSize: 11,
    fontFamily: C.mono,
    fontStyle: 'italic',
  },
  badge: {
    display: 'inline-block',
    padding: '1px 8px',
    borderRadius: 999,
    fontSize: 10,
    fontWeight: 700,
    letterSpacing: '0.05em',
    fontFamily: C.sans,
  },
  badgeDeny: { color: C.deny, background: C.denyDim },
  badgeAllow: { color: C.ok, background: C.okDim },
  kind: { color: '#3370ff', fontSize: 11 },
  proc: { color: C.text },
  target: { color: C.text2 },
  args: { color: C.text3, fontSize: 11 },
  ver: { color: C.text3, fontSize: 11 },
  empty: { padding: '56px 24px', textAlign: 'center', color: C.text3, fontSize: 13, fontFamily: C.sans },

  // ---- 详情面板 ----
  detail: { borderTop: `1px solid ${C.border}`, background: C.surfaceDeep, maxHeight: '42%', overflow: 'auto', display: 'flex', flexDirection: 'column' },
  detailHead: {
    display: 'flex',
    alignItems: 'center',
    gap: 8,
    padding: '6px 12px',
    borderBottom: `1px solid ${C.border}`,
    position: 'sticky',
    top: 0,
    background: C.surfaceDeep,
  },
  detailTitle: { fontSize: 12, fontWeight: 600, fontFamily: C.sans, color: C.text, marginRight: 'auto' },
  btnMini: {
    background: 'transparent',
    color: C.text2,
    border: `1px solid ${C.borderStrong}`,
    borderRadius: 5,
    padding: '2px 10px',
    fontSize: 11,
    fontFamily: C.sans,
    cursor: 'pointer',
  },
  pre: {
    margin: 0,
    padding: 10,
    fontFamily: C.mono,
    fontSize: 11,
    lineHeight: 1.6,
    color: C.text2,
    whiteSpace: 'pre-wrap',
    overflow: 'auto',
  },
};

// FilterDraft 过滤区草稿 (查找按钮/回车 才应用到控制器)
interface FilterDraft {
  result: string; // '' | ALLOW | DENY
  kind: string; // '' | 1..4
  pid: string;
  comm: string;
  target: string;
}

const EMPTY_DRAFT: FilterDraft = { result: '', kind: '', pid: '', comm: '', target: '' };

export const AuditView: FC<AuditViewProps> = (props) => {
  const c = props.controller;
  const s = useSyncExternalStore(c.subscribe, c.getSnapshot);
  const [draft, setDraft] = useState<FilterDraft>(EMPTY_DRAFT);

  useEffect(() => {
    c.start();
    return () => c.dispose();
  }, [c]);

  // apply: 草稿 → 控制器过滤条件
  const apply = (): void => {
    const pid = Number(draft.pid);
    c.setFilter({
      result: draft.result === 'ALLOW' || draft.result === 'DENY' ? draft.result : undefined,
      kinds: draft.kind ? [Number(draft.kind)] : undefined,
      pid: draft.pid && Number.isFinite(pid) && pid > 0 ? pid : undefined,
      comm: draft.comm.trim() || undefined,
      target: draft.target.trim() || undefined,
    });
  };
  const reset = (): void => {
    setDraft(EMPTY_DRAFT);
    c.setFilter({});
  };
  const onKey = (e: { key: string }): void => {
    if (e.key === 'Enter') apply();
  };

  const filtered = s.items.filter((it) => it.type === 'row').length;

  return (
    <div style={styles.root}>
      {/* 顶部状态条: 实时/断线/重连中 + boot ID + 最后 sequence + 丢失/淘汰 */}
      <div style={styles.bar}>
        <span style={styles.chip}>
          <span style={s.bar.connected ? styles.dotLive : styles.dotBad}>
            {s.bar.connected ? '●' : '○'}
          </span>
          {s.bar.stateLabel}
        </span>
        <span style={styles.chip}>boot: {s.bar.daemonBootId ? s.bar.daemonBootId.slice(0, 8) : '-'}</span>
        <span style={styles.chip}>seq: {s.bar.lastSequence}</span>
        <span style={styles.chip}>丢失: {s.bar.droppedCount}</span>
        <span style={styles.chip}>淘汰: {s.bar.evictedCount}</span>
        <span style={{ ...styles.chip, marginLeft: 'auto', border: 'none', background: 'transparent' }}>
          {filtered} 条
        </span>
      </div>
      {!s.bar.connected ? <div style={styles.barHint}>{AUDIT_DISCONNECTED_HINT}</div> : null}

      {/* 过滤 (草稿态, [查找]/回车 应用, [重置] 清空) */}
      <div style={styles.filters}>
        <select
          style={styles.input}
          value={draft.result}
          onChange={(e: { target: { value: string } }) => setDraft({ ...draft, result: e.target.value })}
        >
          <option value="">全部结果</option>
          <option value="ALLOW">ALLOW</option>
          <option value="DENY">DENY</option>
        </select>
        <select
          style={styles.input}
          value={draft.kind}
          onChange={(e: { target: { value: string } }) => setDraft({ ...draft, kind: e.target.value })}
        >
          <option value="">全部类型</option>
          <option value="1">FILE</option>
          <option value="2">EXEC</option>
          <option value="3">NET</option>
          <option value="4">GUARD</option>
        </select>
        <input
          style={{ ...styles.input, width: 72 }}
          placeholder="PID"
          value={draft.pid}
          onChange={(e: { target: { value: string } }) => setDraft({ ...draft, pid: e.target.value })}
          onKeyDown={onKey}
        />
        <input
          style={{ ...styles.input, width: 110 }}
          placeholder="命令名"
          value={draft.comm}
          onChange={(e: { target: { value: string } }) => setDraft({ ...draft, comm: e.target.value })}
          onKeyDown={onKey}
        />
        <input
          style={{ ...styles.input, width: 180 }}
          placeholder="目标路径/IP"
          value={draft.target}
          onChange={(e: { target: { value: string } }) => setDraft({ ...draft, target: e.target.value })}
          onKeyDown={onKey}
        />
        <button style={styles.btnPrimary} onClick={apply}>
          查找
        </button>
        <button style={styles.btnGhost} onClick={reset}>
          重置
        </button>
      </div>

      {/* 事件表: 吸顶表头 + 网格对齐行; 断线保留已有事件, gap 行内提示 */}
      <div style={styles.list}>
        <div style={styles.head}>
          <span style={cell()}>时间</span>
          <span style={cell()}>结果</span>
          <span style={cell()}>类型</span>
          <span style={cell()}>DOMAIN</span>
          <span style={cell()}>进程</span>
          <span style={cell()}>目标</span>
          <span style={cell()}>参数</span>
          <span style={cell()}>版本</span>
        </div>
        {s.items.length === 0 ? <div style={styles.empty}>{AUDIT_EMPTY_TEXT}</div> : null}
        {s.items.map((it, i) =>
          it.type === 'gap' ? (
            <div key={`gap-${i}`} style={styles.gapRow}>
              ⚠ {it.message}
            </div>
          ) : (
            <RowView
              key={it.row.sequence}
              row={it.row}
              selected={s.selected?.sequence === it.row.sequence}
              onSelect={(seq) => c.select(c.eventAt(seq))}
            />
          ),
        )}
      </div>

      {/* 详情: 页内面板, 完整原始字段 + 复制 JSON */}
      {s.selected ? (
        <div style={styles.detail}>
          <div style={styles.detailHead}>
            <span style={styles.detailTitle}>
              事件详情 · #{s.selected.sequence} · {s.selected.comm}
            </span>
            <button style={styles.btnMini} onClick={() => void copyJson(s.selected!)}>
              复制 JSON
            </button>
            <button style={styles.btnMini} onClick={() => c.select(null)}>
              关闭
            </button>
          </div>
          <pre style={styles.pre}>{eventDetailJson(s.selected)}</pre>
        </div>
      ) : null}
    </div>
  );
};

// RowView 单行: 时间 结果 类型 Domain 进程 目标 参数 策略版本
// 悬浮高亮需 JS 状态 (内联样式无 :hover)
const RowView: FC<{
  row: AuditRow;
  selected: boolean;
  onSelect: (sequence: number) => void;
}> = ({ row, selected, onSelect }) => {
  const [hover, setHover] = useState(false);
  const args = row.args.length > 0 ? `[${row.args.join(' ')}]` : '';
  return (
    <div
      style={{
        ...styles.row,
        background: selected ? C.accentDim : hover ? C.surface : undefined,
      }}
      onClick={() => onSelect(row.sequence)}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      <span style={cell({ color: C.text2 })} title={row.time}>
        {row.time.slice(11, 19)}
      </span>
      <span style={cell()}>
        <span style={{ ...styles.badge, ...(row.result === 'DENY' ? styles.badgeDeny : styles.badgeAllow) }}>
          {row.result}
        </span>
      </span>
      <span style={cell(styles.kind)} title={row.kindLabel}>
        {row.kindLabel}
      </span>
      <span style={cell({ color: C.text2 })} title={row.domain || '-'}>
        {row.domain || '-'}
      </span>
      <span style={cell(styles.proc)} title={`${row.comm}(${row.pid})`}>
        {row.comm}({row.pid})
      </span>
      <span style={cell(styles.target)} title={row.target}>
        {row.target}
      </span>
      <span style={cell(styles.args)} title={args}>
        {args}
      </span>
      <span style={cell(styles.ver)}>v{row.policyVersion}.{row.ruleVersion}</span>
    </div>
  );
};

async function copyJson(ev: RpcAuditEvent): Promise<void> {
  const text = eventDetailJson(ev);
  const nav = (globalThis as { navigator?: { clipboard?: { writeText(t: string): Promise<void> } } })
    .navigator;
  if (nav?.clipboard) await nav.clipboard.writeText(text);
}
