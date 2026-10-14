/*
 * Browser half of the CensorPivot plugin.
 *
 * Scope owns collection.  This module intentionally only observes the public
 * SessionBinding.eventSource consumed by the native trajectory view; it does
 * not import, patch, or alter any of the three component plugins.
 */
window.__ModuleLoader__.load({
  id: '@agentcensor/censorpivot',
  factory: (require) => {
    var module = { exports: {} };
    var exports = module.exports;
    Object.defineProperty(exports, Symbol.toStringTag, { value: 'Module' });
    const react = require('react');
    const { useEffect, useState, useSyncExternalStore } = react;
    const h = react.createElement;
    const KEY = 'agentcensor.pivot.preferences.v1';
    const guardColumnDefaults = { time: true, result: true, type: true, domain: true, process: true, target: true, args: true, version: true };
    const defaults = { fs: false, guard: false, scope: true, level: 'L2', guardColumns: guardColumnDefaults };

    function readPreferences() {
      try { const saved = JSON.parse(localStorage.getItem(KEY) || '{}'); return { ...defaults, ...saved, guardColumns: { ...guardColumnDefaults, ...(saved.guardColumns || {}) } }; } catch { return { ...defaults }; }
    }
    let committedPreferences = readPreferences();
    const preferenceListeners = new Set();
    function applyPreferences(next) {
      committedPreferences = { ...next };
      try { localStorage.setItem(KEY, JSON.stringify(next)); } catch {}
      preferenceListeners.forEach((listener) => listener());
    }
    const preferences = {
      get: () => committedPreferences,
      subscribe: (listener) => { preferenceListeners.add(listener); return () => preferenceListeners.delete(listener); },
      apply: applyPreferences,
    };
    function openPivotSettings() {
      const trigger = document.querySelector('button[aria-haspopup="dialog"]');
      if (!trigger) return;
      trigger.click();
      let attempts = 0;
      const selectPivot = () => {
        const pivot = [...document.querySelectorAll('button')].find((button) => button.textContent && button.textContent.trim() === 'Pivot');
        if (pivot) { pivot.click(); return; }
        attempts += 1;
        if (attempts < 10) setTimeout(selectPivot, 16);
      };
      setTimeout(selectPivot, 0);
    }
    function closePivotSettings() {
      const close = [...document.querySelectorAll('button')].find((button) => {
        const label = `${button.getAttribute('aria-label') || ''} ${button.getAttribute('title') || ''} ${button.textContent || ''}`.trim().toLowerCase();
        return label === '×' || label.includes('关闭') || label.includes('close');
      });
      if (close) { close.click(); return; }
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', bubbles: true }));
    }
    function time(ms) { return typeof ms === 'number' ? new Date(ms).toLocaleTimeString() : '—'; }
    function sysTime(raw) {
      const ms = Number(raw) / 1e6;
      return Number.isFinite(ms) ? time(ms) : String(raw ?? '—');
    }
    function dash(value) { return value === undefined || value === null || value === '' ? '—' : String(value); }
    function args(value) {
      if (value == null) return '—';
      const text = typeof value === 'string' ? value : (() => { try { return JSON.stringify(value); } catch { return String(value); } })();
      return text.length > 140 ? text.slice(0, 140) + '…' : text;
    }
    function messageText(content) {
      if (typeof content === 'string') return content.trim();
      if (!Array.isArray(content)) return '';
      return content.map((part) => {
        if (!part) return '';
        if (typeof part === 'string') return part;
        return typeof part.text === 'string' ? part.text : '';
      }).filter(Boolean).join('\n').trim();
    }
    function isRuntimeContext(text) {
      const content = String(text || '').toLowerCase();
      return content.includes('current runtime context')
        || content.includes('runtime-context snapshots')
        || content.includes('current dsh file policy')
        || content.includes('approval policy:');
    }
    function guardKind(kind, op) {
      const base = { 1: 'FILE', 2: 'EXEC', 3: 'NET', 4: 'GUARD' }[Number(kind)] || 'UNKNOWN';
      return op ? `${base}/${op}` : base;
    }
    function rows(events) {
      const active = new Map(), result = [];
      for (const event of Array.isArray(events) ? events : []) {
        const data = event && event.data || {};
        if (event.type === 'tool/call' && data.callId) {
          const row = { id: data.callId, name: data.name || 'tool', arguments: data.arguments, start: event.time, end: null, status: '执行中' };
          active.set(data.callId, row); result.push(row);
        }
        if (event.type === 'tool/result') {
          const id = data.callId || data.message && data.message.source && data.message.source.callId;
          const row = active.get(id);
          if (!row) continue;
          row.end = event.time;
          const content = data.message && data.message.content;
          row.status = data.error || Array.isArray(content) && content.some((x) => x && x.isError) ? '失败' : '成功';
        }
      }
      return result;
    }
    function makeLedger(ctx, sessionId) {
      const listeners = new Set(); let source = null; let cached = [], raw = [];
      const eventList = () => {
        const window = source && source.getSnapshot ? source.getSnapshot() : null;
        const entries = window && Array.isArray(window.entries) ? window.entries : [];
        return entries.filter((entry) => entry && entry.type === 'event' && entry.event && typeof entry.event.type === 'string').map((entry) => entry.event);
      };
      const rebuild = () => { raw = eventList(); cached = rows(raw); listeners.forEach((f) => f()); return cached; };
      const acquire = () => {
        if (source) return source;
        try {
          const binding = ctx.sessions && ctx.sessions.binding && ctx.sessions.binding(sessionId);
          const candidate = binding && binding.eventSource;
          if (!candidate || !candidate.subscribe || !candidate.getSnapshot) return null;
          source = candidate;
          source.subscribe(rebuild);
          return source;
        } catch { return null; }
      };
      const onSessionEvent = () => { if (acquire()) rebuild(); };
      try { ctx.on && ctx.on('session/event', onSessionEvent); } catch {}
      acquire(); rebuild();
      return { subscribe: (f) => { listeners.add(f); return () => listeners.delete(f); }, snapshot: () => cached, raw: () => raw };
    }
    const css = `
      .pivot-root{height:calc(100dvh - 132px);max-height:calc(100dvh - 132px);min-height:0;box-sizing:border-box;padding:14px;overflow:hidden;contain:layout size;background:var(--dsw-alias-bg-layer-1,#fff);color:var(--dsw-alias-label-primary,#1f2329);font:13px system-ui,-apple-system,"Segoe UI","Microsoft YaHei",sans-serif;display:flex;flex-direction:column}.pivot-root>.pivot-head{flex:none}
      .pivot-grid{display:grid;gap:14px;height:0;min-height:0;flex:1}.pivot-grid.triple{grid-template-columns:minmax(0,1fr) minmax(0,1fr);grid-template-rows:minmax(0,1fr) minmax(240px,1.15fr)}.pivot-grid.pair{grid-template-columns:minmax(0,1fr) minmax(0,1fr);grid-template-rows:minmax(0,1fr)}.pivot-grid.stack,.pivot-grid.one{grid-template-columns:1fr;grid-template-rows:repeat(2,minmax(0,1fr))}.pivot-grid.one{grid-template-rows:minmax(0,1fr)}
      .pivot-card{min-width:0;min-height:0;border:1px solid var(--dsw-alias-border-l2,#e5e6eb);border-radius:10px;background:var(--dsw-alias-bg-layer-2,#fff);padding:14px;display:flex;flex-direction:column;overflow:hidden}.pivot-grid.triple .pivot-card.scope{grid-column:1 / -1}.pivot-card.scope{min-height:0}.pivot-grid.expanded{grid-template-columns:minmax(0,1fr);grid-template-rows:minmax(0,1fr)}.pivot-card.pivot-expanded{grid-column:1 / -1;grid-row:1 / -1;animation:pivot-card-expand 180ms cubic-bezier(.2,.8,.2,1) both}.pivot-card.pivot-expanded.is-collapsing{animation:pivot-card-collapse 180ms cubic-bezier(.4,0,1,1) both}@keyframes pivot-card-expand{from{opacity:.35;transform:scale(.97)}to{opacity:1;transform:scale(1)}}@keyframes pivot-card-collapse{from{opacity:1;transform:scale(1)}to{opacity:.35;transform:scale(.97)}}@media(prefers-reduced-motion:reduce){.pivot-card.pivot-expanded,.pivot-card.pivot-expanded.is-collapsing{animation:none}}
      .pivot-head{display:flex;align-items:center;gap:8px;margin-bottom:10px}.pivot-title{font-size:16px;font-weight:650}.pivot-sub{color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px}.pivot-spacer{flex:1}.pivot-button{border:1px solid var(--dsw-alias-border-l2,#d5d7de);background:transparent;border-radius:7px;padding:6px 10px;color:inherit;cursor:pointer}.pivot-button.primary{background:#3370ff;color:#fff;border-color:#3370ff}.pivot-expand-button{margin-left:auto;flex:none;width:30px;height:30px;padding:0;display:inline-flex;align-items:center;justify-content:center;font-size:16px;line-height:1}.pivot-expand-button:hover{color:#2864f0;border-color:#9bb8ff;background:#f3f6ff}
      .pivot-empty{display:flex;flex:1;align-items:center;justify-content:center;color:var(--dsw-alias-label-tertiary,#8f959e);background:var(--dsw-alias-bg-layer-3,#f7f8fa);border-radius:8px}.pivot-table{width:100%;border-collapse:collapse}.pivot-table th{text-align:left;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px;font-weight:500;padding:7px;border-bottom:1px solid var(--dsw-alias-border-l1,#eee)}.pivot-table td{padding:8px 7px;border-bottom:1px solid var(--dsw-alias-border-l1,#eee);vertical-align:middle}.pivot-call{cursor:pointer}.pivot-call:hover{background:var(--dsw-alias-interactive-bg-hover,rgba(0,0,0,.04))}.pivot-detail td{padding:0 7px 10px;background:var(--dsw-alias-bg-layer-3,#f7f8fa)}.pivot-detail-box{padding:9px;border:1px solid var(--dsw-alias-border-l1,#e5e6eb);border-radius:7px;overflow:auto;max-height:280px}.pivot-detail-meta{color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px;margin-bottom:6px}.pivot-sys-table{font-size:12px}.pivot-sys-table td,.pivot-sys-table th{padding:5px 6px}.pivot-code{font-family:ui-monospace,SFMono-Regular,Consolas,monospace;color:var(--dsw-alias-label-secondary,#52575f);max-width:54vw;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.pivot-ok,.pivot-bad,.pivot-run{font-size:11px;font-weight:600;padding:2px 7px;border-radius:999px}.pivot-ok{color:#188a4d;background:#e3f5ea}.pivot-bad{color:#d92d20;background:#fdecec}.pivot-run{color:#b25e09;background:#fdf1e1}
      .pivot-module-head{display:flex;align-items:center;gap:10px}.pivot-module-copy{display:flex;flex-direction:column;gap:2px}.pivot-module-copy .pivot-title{font-size:17px}.pivot-scope-head,.pivot-guard-head{flex:none;padding:0 2px 10px;border-bottom:1px solid var(--dsw-alias-border-l1,#eee)}.pivot-scope-icon{width:23px;height:23px;display:inline-flex;align-items:flex-end;gap:3px;color:#2864f0}.pivot-scope-icon i{display:block;width:4px;border:1.8px solid currentColor;border-radius:2px;background:rgba(40,100,240,.08);box-sizing:border-box}.pivot-scope-icon i:nth-child(1){height:10px}.pivot-scope-icon i:nth-child(2){height:20px}.pivot-scope-icon i:nth-child(3){height:14px}.pivot-guard-icon{position:relative;width:25px;height:27px;background:#2864f0;clip-path:polygon(50% 0,94% 16%,88% 70%,50% 100%,12% 70%,6% 16%)}.pivot-guard-icon:after{content:"";position:absolute;inset:3px;background:var(--dsw-alias-bg-layer-2,#fff);clip-path:polygon(50% 0,94% 16%,88% 70%,50% 100%,12% 70%,6% 16%)}.pivot-scope-scroll{flex:1;min-height:0;overflow:auto;padding:10px 4px 4px;scrollbar-width:thin;scrollbar-color:#b8c2d9 transparent}.pivot-scope-scroll::-webkit-scrollbar,.pivot-guard-list::-webkit-scrollbar{width:7px}.pivot-scope-scroll::-webkit-scrollbar-thumb,.pivot-guard-list::-webkit-scrollbar-thumb{background:#b8c2d9;border-radius:7px}.pivot-message{margin:0 0 9px;border:1px solid var(--dsw-alias-border-l1,#e5e6eb);border-radius:8px;padding:9px 11px}.pivot-message.user{background:#f4f8ff;border-left:3px solid #4e7fff}.pivot-message.assistant{background:var(--dsw-alias-bg-layer-3,#f8f9fb);border-left:3px solid #8c7cff}.pivot-message-meta{display:flex;align-items:center;gap:8px;font-size:12px;font-weight:650;margin-bottom:5px}.pivot-message-meta time{margin-left:auto;color:var(--dsw-alias-label-tertiary,#8f959e);font-weight:400}.pivot-message-text{white-space:pre-wrap;word-break:break-word;line-height:1.55;color:var(--dsw-alias-label-secondary,#52575f)}.pivot-tool{margin:0 0 9px;border:1px solid var(--dsw-alias-border-l1,#e5e6eb);border-radius:8px;overflow:hidden}.pivot-tool-main{display:flex;align-items:center;gap:8px;padding:9px 10px;cursor:pointer;background:var(--dsw-alias-bg-layer-2,#fff)}.pivot-tool-main:hover{background:var(--dsw-alias-interactive-bg-hover,rgba(0,0,0,.04))}.pivot-tool-caret{color:var(--dsw-alias-label-tertiary,#8f959e);width:10px}.pivot-tool-name{font-weight:650}.pivot-tool-args{flex:1;min-width:0;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px}.pivot-tool-time{margin-left:auto;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px;white-space:nowrap}.pivot-tool-main>.pivot-ok,.pivot-tool-main>.pivot-bad,.pivot-tool-main>.pivot-run{flex:none}.pivot-tool-detail{padding:0 10px 10px;background:var(--dsw-alias-bg-layer-3,#f7f8fa)}.pivot-live{margin-left:auto;color:#188a4d;background:#e3f5ea;border-radius:999px;padding:3px 8px;font-size:11px;font-weight:650}.pivot-guard-filters{display:flex;gap:6px;padding:9px 0;flex-wrap:wrap}.pivot-guard-filters input,.pivot-guard-filters select{min-width:0;border:1px solid #d5d7de;border-radius:6px;padding:5px 7px;background:#fff;font-size:12px}.pivot-guard-list{flex:1;min-height:0;overflow:auto;scrollbar-width:thin;scrollbar-color:#b8c2d9 transparent}.pivot-guard-table{min-width:760px;width:100%;border-collapse:collapse;font-size:12px}.pivot-guard-table th{position:sticky;top:0;background:var(--dsw-alias-bg-layer-3,#f7f8fa);z-index:1;text-align:left;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:11px;padding:7px 5px;border-bottom:1px solid var(--dsw-alias-border-l1,#eee)}.pivot-guard-table td{padding:7px 5px;border-bottom:1px solid var(--dsw-alias-border-l1,#eee);white-space:nowrap}.pivot-guard-target{max-width:260px;overflow:hidden;text-overflow:ellipsis;color:var(--dsw-alias-label-secondary,#52575f)}.pivot-guard-mono{font-family:ui-monospace,SFMono-Regular,Consolas,monospace;max-width:180px;overflow:hidden;text-overflow:ellipsis}
      .pivot-fs-icon{position:relative;width:21px;height:15px;border:2px solid #2864f0;border-radius:2px;box-sizing:border-box}.pivot-fs-icon:before{content:"";position:absolute;left:1px;top:-6px;width:8px;height:5px;border:2px solid #2864f0;border-bottom:0;border-radius:2px 2px 0 0}.pivot-fs-empty{flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;border:1px solid var(--dsw-alias-border-l1,#eee);border-radius:7px}.pivot-fs-empty-icon{width:54px;height:42px;border-radius:11px;background:linear-gradient(135deg,#dbe7ff,#eef3ff);position:relative;margin-bottom:18px}.pivot-fs-empty-icon:after{content:"◎";position:absolute;color:#8ba5df;font-size:25px;left:15px;top:6px}.pivot-fs-empty strong{font-size:15px}.pivot-fs-empty span{color:#9aa9c7;margin-top:8px}.pivot-fs-flow{display:flex;align-items:stretch;gap:5px;overflow:auto;padding:0 0 9px}.pivot-fs-flow-step{flex:1;min-width:86px;border:1px solid var(--dsw-alias-border-l1,#e5e6eb);border-radius:6px;padding:6px 7px;background:var(--dsw-alias-bg-layer-3,#f7f8fa)}.pivot-fs-flow-step:last-child{border-color:#2ea121;background:#f1fbf4}.pivot-fs-flow-name{font-size:9px;font-weight:700;letter-spacing:.35px;color:var(--dsw-alias-label-tertiary,#8f959e)}.pivot-fs-flow-value{margin-top:3px;font-size:11px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}.pivot-fs-flow-arrow{align-self:center;color:#8f959e}.pivot-fs-layout{display:grid;grid-template-columns:minmax(0,1fr) minmax(170px,.55fr);gap:10px;flex:1;min-height:0}.pivot-fs-runs{min-height:0;overflow:auto;padding:2px;scrollbar-width:thin;scrollbar-color:#b8c2d9 transparent}.pivot-fs-run{border:1px solid var(--dsw-alias-border-l1,#e5e6eb);border-radius:8px;padding:10px;margin-bottom:10px}.pivot-fs-task{margin:5px 0 8px;line-height:1.45;color:var(--dsw-alias-label-secondary,#52575f);display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}.pivot-fs-world{border-top:1px solid var(--dsw-alias-border-l1,#eee);padding:9px 0}.pivot-fs-world:first-of-type{border-top:0}.pivot-fs-world-head{display:flex;align-items:center;gap:7px}.pivot-fs-letter{display:inline-flex;width:20px;height:20px;border-radius:5px;align-items:center;justify-content:center;color:#fff;font-size:12px;font-weight:700}.pivot-fs-status{margin-left:auto;font-size:10px;font-weight:700;letter-spacing:.25px}.pivot-fs-meta{font-size:11px;color:var(--dsw-alias-label-tertiary,#8f959e);margin:5px 0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}.pivot-fs-actions{display:flex;gap:6px;flex-wrap:wrap}.pivot-fs-actions .pivot-button{padding:4px 8px;font-size:12px}.pivot-fs-side{min-width:0;display:flex;flex-direction:column;border:1px solid var(--dsw-alias-border-l1,#e5e6eb);border-radius:8px;overflow:hidden}.pivot-fs-tabs{display:flex;border-bottom:1px solid var(--dsw-alias-border-l1,#e5e6eb)}.pivot-fs-tabs button{flex:1;border:0;border-bottom:2px solid transparent;background:transparent;padding:7px 3px;font-size:11px;cursor:pointer}.pivot-fs-tabs button.active{color:#2864f0;border-bottom-color:#2864f0;font-weight:650}.pivot-fs-side-body{flex:1;min-height:0;overflow:auto;padding:8px;font-size:11px}.pivot-fs-activity,.pivot-fs-version{display:flex;gap:6px;padding:5px 0;border-bottom:1px solid var(--dsw-alias-border-l1,#eee);min-width:0}.pivot-fs-activity:last-child,.pivot-fs-version:last-child{border-bottom:0}.pivot-fs-dot{width:7px;height:7px;border-radius:50%;background:#8f959e;margin-top:4px;flex:none}.pivot-fs-side-text{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.pivot-fs-mono{font-family:ui-monospace,SFMono-Regular,Consolas,monospace;color:var(--dsw-alias-label-secondary,#52575f)}@media(max-width:900px){.pivot-fs-layout{grid-template-columns:1fr}.pivot-fs-side{min-height:126px}}.pivot-settings{padding:16px;max-width:840px}.pivot-section{border:1px solid var(--dsw-alias-border-l2,#e5e6eb);border-radius:10px;padding:14px;margin:12px 0}.pivot-section h3{margin:0 0 4px;font-size:15px}.pivot-help{margin:0 0 12px;color:var(--dsw-alias-label-tertiary,#8f959e);line-height:1.6}.pivot-setting{display:flex;align-items:center;gap:12px;padding:10px 0;border-top:1px solid var(--dsw-alias-border-l1,#eee)}.pivot-setting:first-of-type{border-top:0}.pivot-setting label{flex:1;font-weight:600}.pivot-setting small{display:block;font-weight:400;color:var(--dsw-alias-label-tertiary,#8f959e);margin-top:3px}.pivot-switch{width:38px;height:22px;accent-color:#3370ff}.pivot-level{display:flex;gap:8px}.pivot-level button{flex:1;padding:8px;border:1px solid #d5d7de;background:#fff;color:#52575f;cursor:pointer}.pivot-level button.active{background:#3370ff;color:#fff;border-color:#3370ff}.pivot-rule{width:100%;min-height:130px;box-sizing:border-box;font:12px ui-monospace,SFMono-Regular,Consolas,monospace;border:1px solid #d5d7de;border-radius:7px;padding:8px;color:#8f959e;background:#f7f8fa}.pivot-actions{display:flex;justify-content:flex-end;gap:8px}.pivot-button:disabled{opacity:.45;cursor:not-allowed}.pivot-status-grid{display:grid;grid-template-columns:120px 1fr;gap:8px 12px;align-items:center}.pivot-status-label{color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px}.pivot-status-value{font:12px ui-monospace,SFMono-Regular,Consolas,monospace}.pivot-status-ok{color:#2ea121;background:rgba(52,199,36,.14);border-radius:999px;padding:2px 10px;font-size:11px;font-weight:650}
    `;
    function ensureStyle() { if (!document.getElementById('censorpivot-style')) { const n = document.createElement('style'); n.id = 'censorpivot-style'; n.textContent = css; document.head.appendChild(n); } }
    function ExpandButton({ expanded, onToggle, label }) {
      const icon = expanded
        ? h('svg', { width: 16, height: 16, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 2, strokeLinecap: 'round', strokeLinejoin: 'round', 'aria-hidden': true }, h('path', { d: 'm8 3 4 4 4-4M3 8l4 4-4 4M16 21l-4-4-4 4M21 16l-4-4 4-4' }))
        : '⤢';
      return h('button', { type: 'button', className: `pivot-button pivot-expand-button${expanded ? ' is-collapse' : ''}`, onClick: onToggle, title: expanded ? `缩小 ${label}` : `放大 ${label}`, 'aria-label': expanded ? `缩小 ${label}` : `放大 ${label}` }, icon)
    }
    function Placeholder({ title, product }) { return h('section', { className: 'pivot-card' }, h('div', { className: 'pivot-head' }, h('span', { className: 'pivot-title' }, title), h('span', { className: 'pivot-sub' }, product)), h('div', { className: 'pivot-empty' }, '暂未接通数据源')) }
    const fsColors = { running: '#1677ff', prepared: '#2ea121', 'validation-failed': '#d92d20', failed: '#d92d20', published: '#2ea121', aborted: '#8f959e', stale: '#d97706', unvalidated: '#6b7280' };
    const fsLabels = { running: '运行中', prepared: '就绪', 'validation-failed': '验证失败', failed: '失败', published: '已发布', aborted: '已放弃', stale: '版本冲突', unvalidated: '待验证' };
    function fsRuns(events) {
      const runs = new Map();
      for (const event of events || []) {
        const data = event && event.data || {}; const runId = data.runId;
        if (event.type === 'exploration-started' && runId) runs.set(runId, { ...data, variants: {}, startedAt: event.time || Date.now() });
        if (!runId || !runs.has(runId)) continue;
        const run = runs.get(runId); const id = data.variantId;
        if (!id) continue;
        const before = run.variants[id] || { variantId: id, activities: [], sysActivities: [] };
        if (event.type === 'variant-running') run.variants[id] = { ...before, ...data, status: 'running' };
        else if (event.type === 'variant-activity') run.variants[id] = { ...before, activities: [...(before.activities || []), data.activity] };
        else if (event.type === 'variant-sys-activity') run.variants[id] = { ...before, sysActivities: [...(before.sysActivities || []), data.activity] };
        else if (event.type === 'variant-prepared') run.variants[id] = { ...before, ...data, status: data.validation && data.validation.status === 'failed' ? 'validation-failed' : data.validation && data.validation.status === 'unvalidated' ? 'unvalidated' : 'prepared' };
        else if (event.type === 'variant-validated') run.variants[id] = { ...before, validation: data.validation, status: data.validation && data.validation.status === 'passed' ? 'prepared' : data.validation && data.validation.status === 'failed' ? 'validation-failed' : 'unvalidated' };
        else if (event.type === 'variant-published') run.variants[id] = { ...before, ...data, status: 'published' };
        else if (event.type === 'variant-aborted') run.variants[id] = { ...before, ...data, status: 'aborted' };
        else if (event.type === 'variant-failed') run.variants[id] = { ...before, ...data, status: 'failed' };
        else if (event.type === 'variant-stale') run.variants[id] = { ...before, ...data, status: 'stale' };
      }
      return [...runs.values()].sort((a, b) => (b.startedAt || 0) - (a.startedAt || 0));
    }
    function FS({ ledger, executeCommand, expanded, collapsing, onToggle }) {
      const events = useSyncExternalStore(ledger.subscribe, ledger.raw); const runs = fsRuns(events);
      const [busy, setBusy] = useState(''); const [message, setMessage] = useState(''); const [side, setSide] = useState('exec');
      const invoke = async (action, runId, variantId) => {
        if (!executeCommand) return setMessage('当前会话的 CensorFS 命令不可用');
        const command = action === 'preview' ? `/censorfs-preview ${runId} ${variantId}` : action === 'validate' ? `/censorfs-validate ${runId} ${variantId}` : action === 'publish' ? `/censorfs-publish ${runId} ${variantId}` : `/censorfs-abort ${runId} ${variantId}`;
        setBusy(`${action}:${runId}:${variantId}`); setMessage('');
        try { const result = await executeCommand(command); if (!result || !result.ok) throw new Error(result && result.error ? result.error.message : '命令执行失败'); const reply = result.value && result.value.result; if (!reply || reply.kind === 'error') throw new Error(reply && reply.text || '命令执行失败'); if (action === 'preview') { const matched = /https?:\/\/[^\s]+/u.exec(reply.text || ''); if (matched) window.open(matched[0].replace(/[),.;]+$/u, ''), '_blank', 'noopener,noreferrer'); } setMessage(reply.text || '操作已完成'); } catch (error) { setMessage(`操作失败：${String(error && error.message || error)}`); } finally { setBusy(''); }
      };
      const world = (run, variant, index) => { const status = variant.status || 'running'; const allowed = ['prepared', 'validation-failed', 'unvalidated', 'stale'].includes(status) && run.fsEnabled !== false; const token = (action) => `${action}:${run.runId}:${variant.variantId}`; return h('article', { className: 'pivot-fs-world', key: `${run.runId}:${variant.variantId}` }, h('div', { className: 'pivot-fs-world-head' }, h('span', { className: 'pivot-fs-letter', style: { background: fsColors[status] || '#6b7280' } }, String.fromCharCode(65 + index)), h('strong', null, `World ${variant.label || variant.variantId}`), h('span', { className: 'pivot-fs-status', style: { color: fsColors[status] || '#6b7280' } }, fsLabels[status] || status.toUpperCase())), variant.strategy ? h('div', { className: 'pivot-fs-meta', title: variant.strategy }, variant.strategy) : null, h('div', { className: 'pivot-fs-meta' }, `${(variant.pathDiff || []).length} 文件变更${variant.durationMs !== undefined ? ` · ${(variant.durationMs / 1000).toFixed(1)}s` : ''}${variant.generationId ? ` · gen/${String(variant.generationId).slice(0, 8)}` : ''}`), h('div', { className: 'pivot-fs-actions' }, h('button', { className: 'pivot-button', disabled: !!busy, onClick: () => void invoke('preview', run.runId, variant.variantId) }, '预览'), allowed ? h('button', { className: 'pivot-button', disabled: !!busy, onClick: () => void invoke('validate', run.runId, variant.variantId) }, '验证') : null, allowed ? h('button', { className: 'pivot-button primary', disabled: !!busy, onClick: () => void invoke('publish', run.runId, variant.variantId) }, busy === token('publish') ? '发布中…' : '发布') : null, allowed ? h('button', { className: 'pivot-button', disabled: !!busy, onClick: () => void invoke('abort', run.runId, variant.variantId) }, '放弃') : null)); };
      if (runs.length) {
        const run = runs[0]; const variants = Object.values(run.variants || {}); const published = variants.find((variant) => variant.status === 'published');
        const ready = variants.filter((variant) => ['prepared', 'unvalidated', 'validation-failed'].includes(variant.status)).length;
        const flow = [['BASELINE', run.expectedHead && run.expectedHead.generation_id ? `gen/${String(run.expectedHead.generation_id).slice(0, 8)}` : 'main'], ['WORLDS', `${variants.length} total`], ['CANDIDATES', `${variants.filter((variant) => variant.candidate).length} created`], ['EVALUATION', ready ? `${ready} ready` : '—'], ['MAIN', published && published.generationId ? `gen/${String(published.generationId).slice(0, 8)}` : published ? 'Published' : 'Waiting']];
        const activities = variants.flatMap((variant) => (variant.activities || []).slice(-5).map((activity) => ({ variant, activity }))).slice(-12);
        const execution = activities.length ? activities.map(({ variant, activity }, index) => h('div', { className: 'pivot-fs-activity', key: `${variant.variantId}:${index}:${activity.at || ''}` }, h('span', { className: 'pivot-fs-dot', style: { background: activity.type && activity.type.includes('write') ? '#2ea121' : activity.type && activity.type.includes('read') ? '#7c5cff' : '#d97706' } }), h('div', { className: 'pivot-fs-side-text', title: `${variant.variantId} · ${activity.tool || activity.type || 'event'}${activity.target ? ` · ${activity.target}` : ''}` }, h('strong', null, `World ${variant.variantId} · `), activity.tool || activity.type || 'event'))) : h('div', { className: 'pivot-empty', style: { minHeight: '90px', fontSize: '11px' } }, '暂无子代理活动');
        const versions = [h('div', { className: 'pivot-fs-version', key: 'base' }, h('span', { className: 'pivot-fs-dot', style: { background: '#24292e' } }), h('div', { className: 'pivot-fs-side-text' }, h('strong', null, 'main '), h('span', { className: 'pivot-fs-mono' }, flow[0][1])))].concat(variants.map((variant) => h('div', { className: 'pivot-fs-version', key: variant.variantId }, h('span', { className: 'pivot-fs-dot', style: { background: fsColors[variant.status] || '#8f959e' } }), h('div', { className: 'pivot-fs-side-text', title: variant.candidate && variant.candidate.candidate_id || variant.generationId || '' }, h('strong', null, `World ${variant.variantId} `), h('span', { className: 'pivot-fs-mono' }, variant.generationId ? `→ gen/${String(variant.generationId).slice(0, 8)}` : variant.candidate && variant.candidate.candidate_id ? `cand/${String(variant.candidate.candidate_id).slice(0, 8)}` : 'running')))));
        return h('section', { className: `pivot-card${expanded ? ' pivot-expanded' : ''}${collapsing ? ' is-collapsing' : ''}` }, h('div', { className: 'pivot-module-head', style: { marginBottom: '10px' } }, h('svg', { width: 25, height: 25, viewBox: '0 0 24 24', fill: 'none', stroke: '#2864f0', strokeWidth: 2, strokeLinecap: 'round', strokeLinejoin: 'round', 'aria-hidden': true }, h('path', { d: 'M3 7.5A2.5 2.5 0 0 1 5.5 5H10l2 2h6.5A2.5 2.5 0 0 1 21 9.5v8A2.5 2.5 0 0 1 18.5 20h-13A2.5 2.5 0 0 1 3 17.5z' })), h('div', { className: 'pivot-module-copy' }, h('span', { className: 'pivot-title' }, 'Worlds'), h('span', { className: 'pivot-sub' }, '并行沙箱环境'))), h(ExpandButton, { expanded, onToggle, label: 'Worlds' }), h('div', { className: 'pivot-fs-task', title: run.task || '' }, run.task || '未提供任务描述'), h('div', { className: 'pivot-fs-flow' }, flow.flatMap(([name, value], index) => [h('div', { className: 'pivot-fs-flow-step', key: name }, h('div', { className: 'pivot-fs-flow-name' }, name), h('div', { className: 'pivot-fs-flow-value', title: value }, value)), index < flow.length - 1 ? h('span', { className: 'pivot-fs-flow-arrow', key: `${name}:arrow` }, '→') : null])), h('div', { className: 'pivot-fs-layout' }, h('div', { className: 'pivot-fs-runs' }, h('article', { className: 'pivot-fs-run' }, h('strong', null, 'Parallel Worlds'), variants.map((variant, index) => world(run, variant, index))), message ? h('div', { className: 'pivot-fs-meta', style: { color: message.startsWith('操作失败') ? '#d92d20' : undefined } }, message) : null), h('aside', { className: 'pivot-fs-side' }, h('div', { className: 'pivot-fs-tabs' }, h('button', { className: side === 'exec' ? 'active' : '', onClick: () => setSide('exec') }, '执行图'), h('button', { className: side === 'version' ? 'active' : '', onClick: () => setSide('version') }, '版本分支')), h('div', { className: 'pivot-fs-side-body' }, side === 'exec' ? execution : versions))));
      }
      return h('section', { className: `pivot-card${expanded ? ' pivot-expanded' : ''}${collapsing ? ' is-collapsing' : ''}` }, h('div', { className: 'pivot-module-head', style: { marginBottom: '12px' } }, h('svg', { width: 25, height: 25, viewBox: '0 0 24 24', fill: 'none', stroke: '#2864f0', strokeWidth: 2, strokeLinecap: 'round', strokeLinejoin: 'round', 'aria-hidden': true }, h('path', { d: 'M3 7.5A2.5 2.5 0 0 1 5.5 5H10l2 2h6.5A2.5 2.5 0 0 1 21 9.5v8A2.5 2.5 0 0 1 18.5 20h-13A2.5 2.5 0 0 1 3 17.5z' })), h('div', { className: 'pivot-module-copy' }, h('span', { className: 'pivot-title' }, 'Worlds'), h('span', { className: 'pivot-sub' }, '并行沙箱环境')), h(ExpandButton, { expanded, onToggle, label: 'Worlds' })), !runs.length ? h('div', { className: 'pivot-fs-empty' }, h('div', { className: 'pivot-fs-empty-icon' }), h('strong', null, '在会话中使用 `/explore` 调用'), h('span', null, 'CensorFS / Worlds 已启用，尚未创建任何世界。')) : h('div', { className: 'pivot-fs-runs' }, runs.map((run) => h('article', { className: 'pivot-fs-run', key: run.runId }, h('strong', null, 'Parallel Worlds'), h('div', { className: 'pivot-fs-task', title: run.task || '' }, run.task || '未提供任务描述'), Object.values(run.variants || {}).map((variant, index) => world(run, variant, index)))), message ? h('div', { className: 'pivot-fs-meta', style: { color: message.startsWith('操作失败') ? '#d92d20' : undefined } }, message) : null));
    }
    function Scope({ ledger, sessionId, expanded, collapsing, onToggle }) {
      const list = useSyncExternalStore(ledger.subscribe, ledger.snapshot);
      const events = useSyncExternalStore(ledger.subscribe, ledger.raw);
      const [open, setOpen] = useState({});
      const [details, setDetails] = useState({});
      const load = async (callId) => {
        if (details[callId]) return;
        setDetails((previous) => ({ ...previous, [callId]: { loading: true } }));
        try {
          const response = await fetch(`/censorpivot/call?session=${encodeURIComponent(sessionId || '')}&call=${encodeURIComponent(callId)}`, { headers: { accept: 'application/json' } });
          const body = await response.json();
          setDetails((previous) => ({ ...previous, [callId]: body && body.ok ? body : { error: (body && body.error) || `HTTP ${response.status}` } }));
        } catch (error) {
          setDetails((previous) => ({ ...previous, [callId]: { error: String(error && error.message || error) } }));
        }
      };
      const toggle = (callId) => {
        if (!details[callId]) void load(callId);
        setOpen((previous) => ({ ...previous, [callId]: !previous[callId] }));
      };
      const detail = (callId) => {
        const state = details[callId];
        if (!state || state.loading) return h('div', { className: 'pivot-detail-meta' }, '正在从 censorscoped 读取系统调用…');
        if (state.error) return h('div', { className: 'pivot-detail-meta' }, `系统调用获取失败：${state.error}`);
        const events = Array.isArray(state.events) ? state.events : [];
        if (!events.length) return h('div', { className: 'pivot-detail-meta' }, '该工具调用未采集到系统调用事件。');
        const shown = events.slice(0, 300);
        return h('div', null,
          h('div', { className: 'pivot-detail-meta' }, `censorscoped 原始系统调用：${events.length} 条${events.length > shown.length ? '（仅显示前 300 条）' : ''}`),
          h('div', { className: 'pivot-detail-box' }, h('table', { className: 'pivot-table pivot-sys-table' },
            h('thead', null, h('tr', null, h('th', null, '时间'), h('th', null, '进程'), h('th', null, '类型/操作'), h('th', null, '目标'), h('th', null, '大小'), h('th', null, '结果'))),
            h('tbody', null, shown.map((event, index) => h('tr', { key: `${event.event_id || 'event'}:${index}` },
              h('td', null, sysTime(event.observed_at)),
              h('td', null, dash(event.host_pid ?? event.process_id)),
              h('td', null, `${event.kind_name || 'unknown'}${event.operation && event.operation !== event.kind_name ? `/${event.operation}` : ''}`),
              h('td', { className: 'pivot-code', title: dash(event.path || event.endpoint || event.stream || (event.fd !== undefined ? `fd ${event.fd}` : null)) }, dash(event.path || event.endpoint || event.stream || (event.fd !== undefined ? `fd ${event.fd}` : null))),
              h('td', null, Number(event.size) > 0 ? dash(event.size) : '—'),
              h('td', null, dash(event.result)),
            ))),
          )),
        );
      };
      const byId = new Map(list.map((row) => [row.id, row]));
      const timeline = [];
      for (const event of events) {
        const data = event && event.data || {};
        if (event.type === 'user/message') {
          const text = messageText(data.content || data.message && data.message.content);
          if (text && !isRuntimeContext(text)) timeline.push({ kind: 'user', time: event.time, text });
        } else if (event.type === 'assistant/message') {
          const text = messageText(data.content || data.message && data.message.content);
          if (text) timeline.push({ kind: 'assistant', time: event.time, text });
        } else if (event.type === 'tool/call' && data.callId) {
          timeline.push({ kind: 'tool', row: byId.get(data.callId) || { id: data.callId, name: data.name || 'tool', arguments: data.arguments, start: event.time, status: '执行中' } });
        }
      }
      const message = (item, index) => h('article', { key: `${item.kind}:${item.time}:${index}`, className: `pivot-message ${item.kind}` },
        h('div', { className: 'pivot-message-meta' }, item.kind === 'user' ? '用户' : '助手', h('time', null, time(item.time))),
        h('div', { className: 'pivot-message-text' }, item.text),
      );
      const tool = (row) => h('article', { key: `tool:${row.id}`, className: 'pivot-tool' },
        h('div', { className: 'pivot-tool-main', onClick: () => toggle(row.id), title: '展开 censorscoped 采集的系统调用' },
          h('span', { className: 'pivot-tool-caret' }, open[row.id] ? '▾' : '▸'),
          h('span', null, '⚙'), h('span', { className: 'pivot-tool-name' }, row.name),
          h('span', { className: 'pivot-tool-args pivot-code', title: args(row.arguments) }, args(row.arguments)),
          h('span', { className: 'pivot-tool-time' }, time(row.start)),
          h('span', { className: row.status === '成功' ? 'pivot-ok' : row.status === '失败' ? 'pivot-bad' : 'pivot-run' }, row.status),
        ),
        open[row.id] ? h('div', { className: 'pivot-tool-detail' }, detail(row.id)) : null,
      );
      const body = timeline.length
        ? h('div', { className: 'pivot-scope-scroll' }, timeline.map((item, index) => item.kind === 'tool' ? tool(item.row) : message(item, index)))
        : h('div', { className: 'pivot-empty' }, '当前会话尚无可展示的 Scope 事件。');
      return h('section', { className: `pivot-card scope${expanded ? ' pivot-expanded' : ''}${collapsing ? ' is-collapsing' : ''}` },
        h('div', { className: 'pivot-scope-head' }, h('div', { className: 'pivot-module-head' }, h('span', { className: 'pivot-scope-icon', 'aria-hidden': true }, h('i'), h('i'), h('i')), h('div', { className: 'pivot-module-copy' }, h('span', { className: 'pivot-title' }, 'Scope'), h('span', { className: 'pivot-sub' }, '会话工具调用观测')), h(ExpandButton, { expanded, onToggle, label: 'Scope' }))),
        body,
      );
    }
    function Guard({ connection, columns, expanded, collapsing, onToggle }) {
      const [state, setState] = useState({ loading: true, entries: [], status: null, error: '' });
      const [draft, setDraft] = useState({ result: '', kind: '', pid: '', comm: '', target: '' });
      const [filter, setFilter] = useState({});
      useEffect(() => {
        let active = true;
        const refresh = async () => {
          if (!connection || !connection.rpc || !connection.rpc.call) {
            if (active) setState({ loading: false, entries: [], status: null, error: 'Guard 连接不可用' });
            return;
          }
          try {
            const response = await connection.rpc.call('/censorguard-read', 'audit/snapshot', { afterSequence: 0, limit: 100 });
            if (!response || !response.ok) throw new Error(response && response.error && response.error.message || '审计服务不可用');
            const snapshot = response.value || {};
            const entries = (Array.isArray(snapshot.entries) ? snapshot.entries : []).filter((entry) => entry && entry.type === 'event' && entry.event).reverse();
            if (active) setState({ loading: false, entries, status: snapshot.status || null, error: '' });
          } catch (error) {
            if (active) setState({ loading: false, entries: [], status: null, error: String(error && error.message || error) });
          }
        };
        void refresh();
        const timer = setInterval(refresh, 2000);
        return () => { active = false; clearInterval(timer); };
      }, [connection]);
      const live = state.status && state.status.state === 'live';
      const labels = { time: '时间', result: '结果', type: '类型', domain: 'DOMAIN', process: '进程', target: '目标', args: '参数', version: '版本' };
      const visible = Object.keys(labels).filter((key) => !columns || columns[key] !== false);
      const cell = (key, event) => key === 'time' ? time(Date.parse(event.ts)) : key === 'result' ? h('span', { className: event.allowed ? 'pivot-ok' : 'pivot-bad' }, event.allowed ? 'ALLOW' : 'DENY') : key === 'type' ? guardKind(event.kind, event.op) : key === 'domain' ? h('span', { className: 'pivot-guard-mono', title: event.domain || '—' }, event.domain || '—') : key === 'process' ? h('span', { className: 'pivot-guard-mono', title: `${event.comm || '—'}(${event.pid || '—'})` }, `${event.comm || '—'}(${event.pid || '—'})`) : key === 'target' ? h('span', { className: 'pivot-guard-target', title: event.detail || '—' }, event.detail || '—') : key === 'args' ? h('span', { className: 'pivot-guard-mono', title: Array.isArray(event.args) ? event.args.join(' ') : '' }, Array.isArray(event.args) && event.args.length ? `[${event.args.join(' ')}]` : '—') : `v${event.policyVersion || 0}.${event.ruleVersion || 0}`;
      const shown = state.entries.filter(({ event }) => (!filter.result || String(event.allowed ? 'ALLOW' : 'DENY') === filter.result) && (!filter.kind || String(event.kind) === filter.kind) && (!filter.pid || String(event.pid) === filter.pid || String(event.tgid) === filter.pid) && (!filter.comm || String(event.comm || '') === filter.comm) && (!filter.target || String(event.detail || '').includes(filter.target)));
      const applyFilter = () => setFilter({ result: draft.result, kind: draft.kind, pid: draft.pid.trim(), comm: draft.comm.trim(), target: draft.target.trim() });
      const resetFilter = () => { const empty = { result: '', kind: '', pid: '', comm: '', target: '' }; setDraft(empty); setFilter({}); };
      const body = state.loading
        ? h('div', { className: 'pivot-empty' }, '正在读取 Guard 审计事件…')
        : state.error
          ? h('div', { className: 'pivot-empty' }, `Guard 数据获取失败：${state.error}`)
          : !shown.length
            ? h('div', { className: 'pivot-empty' }, '尚无当前 DSH Domain 的 Guard 审计事件。')
            : h('div', { className: 'pivot-guard-list' }, h('table', { className: 'pivot-guard-table' },
              h('thead', null, h('tr', null, visible.map((key) => h('th', { key }, labels[key])))),
              h('tbody', null, shown.map(({ event }) => h('tr', { key: event.sequence },
                visible.map((key) => h('td', { key }, cell(key, event))),
              ))),
            ));
      return h('section', { className: `pivot-card${expanded ? ' pivot-expanded' : ''}${collapsing ? ' is-collapsing' : ''}` },
        h('div', { className: 'pivot-guard-head' }, h('div', { className: 'pivot-module-head' }, h('span', { className: 'pivot-guard-icon', 'aria-hidden': true }), h('div', { className: 'pivot-module-copy' }, h('span', { className: 'pivot-title' }, 'Guard'), h('span', { className: 'pivot-sub' }, '实时安全审计')), live ? h('span', { className: 'pivot-live' }, '● LIVE') : null, h(ExpandButton, { expanded, onToggle, label: 'Guard' })), h('div', { className: 'pivot-guard-filters' },
          h('select', { value: draft.result, onChange: (e) => setDraft({ ...draft, result: e.target.value }) }, h('option', { value: '' }, '全部结果'), h('option', { value: 'ALLOW' }, 'ALLOW'), h('option', { value: 'DENY' }, 'DENY')),
          h('select', { value: draft.kind, onChange: (e) => setDraft({ ...draft, kind: e.target.value }) }, h('option', { value: '' }, '全部类型'), h('option', { value: '1' }, 'FILE'), h('option', { value: '2' }, 'EXEC'), h('option', { value: '3' }, 'NET'), h('option', { value: '4' }, 'GUARD')),
          h('input', { placeholder: 'PID', value: draft.pid, onChange: (e) => setDraft({ ...draft, pid: e.target.value }), onKeyDown: (e) => e.key === 'Enter' && applyFilter() }),
          h('input', { placeholder: '命令名', value: draft.comm, onChange: (e) => setDraft({ ...draft, comm: e.target.value }), onKeyDown: (e) => e.key === 'Enter' && applyFilter() }),
          h('input', { placeholder: '目标路径/IP', value: draft.target, onChange: (e) => setDraft({ ...draft, target: e.target.value }), onKeyDown: (e) => e.key === 'Enter' && applyFilter() }),
          h('button', { className: 'pivot-button primary', onClick: applyFilter }, '查找'), h('button', { className: 'pivot-button', onClick: resetFilter }, '重置'))),
        body,
      );
    }
    function PivotView(props) {
      ensureStyle(); const pref = useSyncExternalStore(preferences.subscribe, preferences.get); const ledger = props.ledger; const [expanded, setExpanded] = useState(null); const [collapsing, setCollapsing] = useState(null);
      const enabled = [['fs', 'FS'], ['guard', 'Guard'], ['scope', 'Scope']].filter(([key]) => pref[key]);
      const activeExpanded = expanded && pref[expanded] ? expanded : null;
      const toggleExpanded = (module) => {
        if (expanded !== module) { setCollapsing(null); setExpanded(module); return; }
        setCollapsing(module);
        setTimeout(() => { setExpanded(null); setCollapsing(null); }, 180);
      };
      if (!enabled.length) return h('div', { className: 'pivot-root' }, h('div', { className: 'pivot-empty' }, 'Pivot 模块均已关闭。请在设置中启用至少一个模块。'));
      const grid = activeExpanded ? 'pivot-grid expanded' : enabled.length === 1 ? 'pivot-grid one' : pref.scope && pref.fs && pref.guard ? 'pivot-grid triple' : pref.scope ? 'pivot-grid stack' : 'pivot-grid pair';
      return h('div', { className: 'pivot-root' },
        !activeExpanded ? h('div', { className: 'pivot-head' }, h('span', { className: 'pivot-spacer' }), h('button', { type: 'button', className: 'pivot-button', onClick: openPivotSettings, title: '打开 Pivot 设置' }, '⚙ 设置')) : null,
        h('div', { className: grid }, (!activeExpanded || activeExpanded === 'fs') && pref.fs ? h(FS, { ledger, executeCommand: props.executeCommand, expanded: activeExpanded === 'fs', collapsing: collapsing === 'fs', onToggle: () => toggleExpanded('fs') }) : null, (!activeExpanded || activeExpanded === 'guard') && pref.guard ? h(Guard, { connection: props.connection, columns: pref.guardColumns, expanded: activeExpanded === 'guard', collapsing: collapsing === 'guard', onToggle: () => toggleExpanded('guard') }) : null, (!activeExpanded || activeExpanded === 'scope') && pref.scope ? h(Scope, { ledger, sessionId: props.sessionId, expanded: activeExpanded === 'scope', collapsing: collapsing === 'scope', onToggle: () => toggleExpanded('scope') }) : null));
    }
    function GuardPolicy({ connection }) {
      const [current, setCurrent] = useState(''); const [draft, setDraft] = useState(''); const [version, setVersion] = useState(0); const [status, setStatus] = useState(null); const [busy, setBusy] = useState(false); const [message, setMessage] = useState('');
      const canWrite = !!(connection && connection.isLoopback);
      const read = async () => { if (!connection || !connection.rpc) return setMessage('Guard 连接不可用'); setBusy(true); try { const [r, s] = await Promise.all([connection.rpc.call('/censorguard-read', 'policy', {}), connection.rpc.call('/censorguard-read', 'status', {})]); if (!r.ok) throw new Error(r.error && r.error.message); const p = r.value || {}; setCurrent(p.policyYaml || ''); setDraft(p.policyYaml || ''); setVersion(p.version || 0); if (s && s.ok) setStatus(s.value || null); setMessage('已同步'); } catch (e) { setMessage(`读取失败：${String(e.message || e)}`); } finally { setBusy(false); } };
      useEffect(() => { void read(); }, [connection]);
      const admin = async (endpoint) => { if (!canWrite) return setMessage('当前访问仅可读取 Guard 策略'); setBusy(true); try { const r = await connection.rpc.call('/censorguard-admin', endpoint, { policyYaml: draft, expectedVersion: version }); if (!r.ok) throw new Error(r.error && r.error.message); setMessage(endpoint === 'policy/validate' ? '校验通过' : '已保存并重新加载'); if (endpoint === 'policy/apply') await read(); } catch (e) { setMessage(`操作失败：${String(e.message || e)}`); } finally { setBusy(false); } };
      const dirty = draft !== current;
      const domain = status && Array.isArray(status.domains) ? status.domains[0] : null;
      return h('section', { className: 'pivot-section' }, h('h3', null, '保护状态'), status ? h('div', { className: 'pivot-status-grid' }, h('span', { className: 'pivot-status-label' }, 'daemon 连接'), h('span', { className: 'pivot-status-ok' }, '已连接'), h('span', { className: 'pivot-status-label' }, 'Hook 健康'), h('span', { className: 'pivot-status-ok' }, status.hooksHealthy ? '正常' : '异常'), h('span', { className: 'pivot-status-label' }, '当前 Domain'), h('span', { className: 'pivot-status-value' }, `${domain ? domain.name : '未归属'}${domain ? ` (id=${domain.id})` : ''}`), h('span', { className: 'pivot-status-label' }, '策略组'), h('span', { className: 'pivot-status-value' }, domain ? domain.group : '—'), h('span', { className: 'pivot-status-label' }, '策略版本'), h('span', { className: 'pivot-status-value' }, `v${domain ? domain.version : version} · reloadGen ${status.reloadGen || 0}`), h('span', { className: 'pivot-status-label' }, 'daemon boot'), h('span', { className: 'pivot-status-value' }, status.daemonBootId || '—')) : h('p', { className: 'pivot-help' }, '正在读取保护状态…'), h('h3', { style: { marginTop: '16px' } }, 'Guard 拦截规则'), h('p', { className: 'pivot-help' }, '读取并编辑当前绑定的 CensorGuard 策略组。'), h('textarea', { className: 'pivot-rule', value: draft, disabled: busy || !canWrite, onChange: (e) => setDraft(e.target.value) }), h('div', { className: 'pivot-actions', style: { marginTop: '10px', alignItems: 'center' } }, h('button', { className: 'pivot-button', disabled: busy, onClick: () => void read() }, '重新读取'), h('button', { className: 'pivot-button', disabled: busy || !canWrite, onClick: () => void admin('policy/validate') }, '校验策略'), h('button', { className: 'pivot-button primary', disabled: busy || !canWrite || !dirty, onClick: () => void admin('policy/apply') }, '保存并重新加载'), h('span', { className: 'pivot-sub' }, message)));
    }
    function PivotSettings(props) {
      ensureStyle(); const saved = useSyncExternalStore(preferences.subscribe, preferences.get); const [pref, setPref] = useState(saved); const set = (change) => setPref((current) => ({ ...current, ...change }));
      const dirty = JSON.stringify(pref) !== JSON.stringify(saved);
      return h('div', { className: 'pivot-settings' }, h('h2', null, 'Pivot 设置'),
        h('section', { className: 'pivot-section' }, h('h3', null, '模块开关'), h('p', { className: 'pivot-help' }, '控制 Pivot 中各模块的展示。关闭后不会显示对应区域，其余区域会自动拉伸。'), ['fs', 'guard', 'scope'].map((key) => h('div', { className: 'pivot-setting', key }, h('label', null, key === 'fs' ? 'CensorFS' : key === 'guard' ? 'CensorGuard' : 'CensorScope', h('small', null, key === 'scope' ? '展示由 CensorScope 采集的会话工具调用。' : '首版保留区域，数据接入待后续完成。')), h('input', { className: 'pivot-switch', type: 'checkbox', checked: !!pref[key], onChange: (e) => set({ [key]: e.target.checked }) })))),
        h('section', { className: 'pivot-section' }, h('h3', null, 'Scope 观测等级'), h('p', { className: 'pivot-help' }, '保存为 Pivot 本地展示偏好；不改写 Scope daemon 的采集配置。'), h('div', { className: 'pivot-level' }, ['L1（宽松）', 'L2（标准）', 'L3（严格）'].map((label, i) => { const v = `L${i + 1}`; return h('button', { key: v, className: pref.level === v ? 'active' : '', onClick: () => set({ level: v }) }, label); }))),
        h('section', { className: 'pivot-section' }, h('h3', null, 'Guard 标题栏选项'), h('p', { className: 'pivot-help' }, '勾选 Guard 审计表格中需要展示的列。'), Object.entries({ time: '时间', result: '结果', type: '类型', domain: 'DOMAIN', process: '进程', target: '目标', args: '参数', version: '版本' }).map(([key, label]) => h('label', { key, style: { display: 'inline-flex', gap: '6px', margin: '0 14px 8px 0', alignItems: 'center' } }, h('input', { type: 'checkbox', checked: pref.guardColumns[key] !== false, onChange: (e) => set({ guardColumns: { ...pref.guardColumns, [key]: e.target.checked } }) }), label))),
        h(GuardPolicy, { connection: props.connection }),
        h('div', { className: 'pivot-actions' }, h('button', { type: 'button', className: 'pivot-button', disabled: !dirty, onClick: () => { preferences.apply(pref); closePivotSettings(); } }, '应用')));
    }
    const inject = ['slots', 'sessions', 'connection', 'remote', 'remote.commands'];
    function apply(ctx) {
      ctx.slots.inject('conversation.view', () => ctx.slots.register({ name: 'conversation.view', id: 'pivot', order: 15, label: () => 'Pivot', inject: (sessionId) => ({ ledger: makeLedger(ctx, sessionId), sessionId, connection: ctx.connection, executeCommand: (line) => ctx.remote.commands.execute(sessionId, line, []) }) }, PivotView));
      ctx.slots.inject('settings.section', () => ctx.slots.register({ name: 'settings.section', id: 'pivot', order: 110, label: 'Pivot', inject: () => ({ connection: ctx.connection }) }, PivotSettings));
    }
    module.exports = { name: 'censorpivot-ui', inject, apply };
    return module.exports;
  },
});
