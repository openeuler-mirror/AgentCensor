/**
 * Browser client half for censorscope-ui (hand-authored __ModuleLoader__ artifact).
 *
 * Registers a 'conversation.view' tab rendering the session event window the
 * native trajectory view consumes (SessionBinding.eventSource) as a
 * turn-grouped, tool-call ledger. No dsh source changes; native UI facts in the
 * events (request model, per-message token usage) render from that same data.
 *
 * censorscopeBuildRows stays standalone so scripts/check-ledger.mjs exercises
 * the exact tool-call pairing logic.
 */
window.__ModuleLoader__.load({
  id: "censorscope-ui",
  factory: (require) => {
    var module = { exports: {} };
    var exports = module.exports;
    Object.defineProperty(exports, Symbol.toStringTag, { value: "Module" });

    const react = require("react");
    const { useSyncExternalStore, useState } = react;

    function censorscopeBuildRows(events) {
      const byCall = new Map();
      const rows = [];
      const source = Array.isArray(events) ? events : [];
      for (const ev of source) {
        const data = ev && ev.data ? ev.data : {};
        if (ev.type === "tool/call") {
          const callId = data.callId;
          if (typeof callId !== "string" || callId === "") continue;
          const row = {
            kind: "tool",
            callId,
            name: data.name || "",
            argsRaw: data.arguments,
            turn: data.turn,
            step: data.step,
            startTime: ev.time,
            endTime: null,
            durationMs: null,
            status: "running",
            seq: ev.seq,
          };
          byCall.set(callId, row);
          rows.push(row);
        } else if (ev.type === "tool/result") {
          const callId =
            (data.message && data.message.source && data.message.source.callId) ||
            data.callId;
          if (typeof callId !== "string" || callId === "") continue;
          const row = byCall.get(callId);
          if (row === undefined) continue;
          row.endTime = ev.time;
          row.durationMs =
            typeof row.startTime === "number" && typeof ev.time === "number"
              ? Math.max(0, ev.time - row.startTime)
              : null;
          const content = Array.isArray(data.message && data.message.content)
            ? data.message.content
            : [];
          const isError =
            data.error !== undefined ||
            content.some((block) => block && block.isError === true);
          row.status = isError ? "error" : "success";
          row.resultError = isError
            ? (data.error && data.error.message) || "error"
            : null;
          row.resultSeq = ev.seq;
        }
      }
      return rows;
    }

    function fmtTime(ms) {
      if (typeof ms !== "number") return "—";
      try {
        return new Date(ms).toLocaleTimeString();
      } catch {
        return "—";
      }
    }
    function fmtArgs(raw) {
      if (raw === undefined || raw === null) return "";
      if (typeof raw === "string") return raw.length > 160 ? raw.slice(0, 160) + "…" : raw;
      try {
        const text = JSON.stringify(raw);
        return text.length > 160 ? text.slice(0, 160) + "…" : text;
      } catch {
        return String(raw);
      }
    }
    function summarizeText(text, limit) {
      const flat = String(text || "").replace(/\s+/g, " ").trim();
      return flat.length > limit ? flat.slice(0, limit) + "…" : flat;
    }
    function blocksText(content) {
      if (!Array.isArray(content)) return typeof content === "string" ? content : "";
      return content
        .map((block) => (block && typeof block.text === "string" ? block.text : ""))
        .join("\n");
    }
    function usageSummary(usage) {
      if (!usage || typeof usage !== "object") return "";
      const parts = [];
      if (typeof usage.inputTokens === "number") parts.push("in " + usage.inputTokens);
      if (typeof usage.outputTokens === "number") parts.push("out " + usage.outputTokens);
      if (typeof usage.cacheReadTokens === "number" && usage.cacheReadTokens > 0)
        parts.push("cache " + usage.cacheReadTokens);
      if (typeof usage.reasoningTokens === "number" && usage.reasoningTokens > 0)
        parts.push("reason " + usage.reasoningTokens);
      return parts.join(" · ");
    }

    const inject = ["slots", "sessions"];

    function buildTimeline(events, toolRows) {
      const rowsByCall = new Map(toolRows.map((row) => [row.callId, row]));
      const items = [];
      let pendingModel = null;
      const pushTurn = (ev, turn) => {
        items.push({
          kind: "turn",
          turn,
          time: ev && ev.time,
          model: pendingModel || null,
        });
        pendingModel = null;
      };
      let seenTurn = new Set();
      for (const ev of events) {
        const type = ev && ev.type;
        const data = (ev && ev.data) || {};
        const time = typeof ev.time === "number" ? ev.time : null;
        switch (type) {
          case "request/header": {
            const config = data.header && data.header.config;
            if (config && config.model) {
              pendingModel = { provider: config.provider || "", model: config.model };
            }
            break;
          }
          case "turn/start": {
            const turn = data.turn;
            if (typeof turn === "number" && !seenTurn.has(turn)) {
              seenTurn.add(turn);
              pushTurn(ev, turn);
            }
            break;
          }
          case "user/message": {
            items.push({
              kind: "user",
              turn: data.turn,
              time,
              text: summarizeText(
                blocksText(data.content) || blocksText(data.message && data.message.content),
                500,
              ),
              full: blocksText(data.content) || blocksText(data.message && data.message.content),
            });
            break;
          }
          case "assistant/message": {
            const content = Array.isArray(data.message && data.message.content)
              ? data.message.content
              : [];
            items.push({
              kind: "assistant",
              turn: data.turn,
              step: data.step,
              time,
              usage: usageSummary(data.usage),
              content,
              textBlocks: content.filter((b) => b && b.type === "text"),
              toolCallBlocks: content.filter((b) => b && b.type === "tool-call"),
            });
            break;
          }
          case "tool/call": {
            const callId = data.callId;
            if (typeof callId !== "string" || callId === "") break;
            const row = rowsByCall.get(callId);
            items.push({
              kind: "tool",
              callId,
              row: row || {
                kind: "tool",
                callId,
                name: data.name || "?",
                argsRaw: data.arguments,
                turn: data.turn,
                step: data.step,
                startTime: time,
                status: "running",
                durationMs: null,
              },
              time,
              inAssistant: false,
            });
            break;
          }
          case "tool/result":
          case "session/title":
          case "step/start":
          case "step/end":
          case "turn/end":
          case "assistant/chunk":
          case "tool-call-chunks":
          case "reasoning-chunks":
          case "text-chunks":
          case "request/context":
          case "session":
          case "session/end-seed":
          case "agent/inbox/spliced":
          case "permission/preset":
          case "sandbox/mode":
          case "approval/policy":
          default:
            break;
        }
      }
      // Assistant tool-call blocks without a tool/call event fall back to appended rows.
      const fallback = [];
      for (const item of items) {
        if (item.kind === "assistant" && Array.isArray(item.toolCallBlocks)) {
          for (const block of item.toolCallBlocks) {
            const callId = block.callId;
            if (typeof callId !== "string") continue;
            if (rowsByCall.has(callId)) continue;
            fallback.push({
              kind: "tool",
              callId,
              row: {
                kind: "tool",
                callId,
                name: block.name || "?",
                argsRaw: block.arguments,
                status: "running",
                durationMs: null,
              },
            });
          }
        }
      }
      if (fallback.length > 0) {
        items.push({ kind: "fallback", rows: fallback });
      }
      return items;
    }

    /* Styling mirrors native trajectory via the same theme-adaptive dsw tokens. */
    const STYLE_ID = "censorscope-ui-styles";
    const STYLE_TEXT = [
      ".censorscope-root{box-sizing:border-box;height:100%;display:flex;flex-direction:column;overflow:hidden;",
      "color:var(--dsw-alias-label-primary,#1f2329);background:var(--dsw-alias-bg-layer-1,#ffffff);",
      "font-family:var(--ds-font-family,-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif);font-size:13px}",
      ".censorscope-head{display:flex;align-items:baseline;gap:10px;padding:10px 16px 4px;flex:none}",
      ".censorscope-title{font-size:14px;font-weight:600;color:var(--dsw-alias-label-primary,#1f2329)}",
      ".censorscope-sub{color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px}",
      ".censorscope-scroll{flex:1;min-height:0;overflow:auto;padding:6px 12px 18px}",
      ".censorscope-empty{padding:22px 14px;color:var(--dsw-alias-label-secondary,#646a73);font-size:13px;line-height:1.7}",
      ".censorscope-turn{display:flex;align-items:center;gap:10px;margin:14px 2px 4px;color:var(--dsw-alias-label-secondary,#646a73);font-size:12px;font-weight:600}",
      ".censorscope-turnMeta{display:inline-flex;gap:8px;font-weight:400;color:var(--dsw-alias-label-tertiary,#8f959e)}",
      ".censorscope-turnLine{flex:1;height:1px;background:var(--dsw-alias-border-l1,rgba(0,0,0,.08))}",
      ".censorscope-block{margin:4px 0;border-radius:8px;border:0.5px solid var(--dsw-alias-border-l4,rgba(0,0,0,.08));background:var(--dsw-alias-bg-layer-3,#ffffff)}",
      ".censorscope-user{padding:7px 10px}",
      ".censorscope-assistant{padding:6px 10px}",
      ".censorscope-blockHead{display:flex;align-items:center;gap:8px;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px;margin-bottom:3px}",
      ".censorscope-tagUser{color:var(--dsw-alias-state-success-primary,#188a4d)}",
      ".censorscope-tagAssistant{color:var(--dsw-alias-state-business-primary,#2f6bff)}",
      ".censorscope-text{white-space:pre-wrap;color:var(--dsw-alias-label-secondary,#52575f);font-size:13px;line-height:1.6;max-height:180px;overflow:hidden;word-break:break-word}",
      ".censorscope-reasoning{color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px;line-height:1.6;white-space:pre-wrap;max-height:96px;overflow:hidden}",
      ".censorscope-usage{margin-left:auto;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:11px}",
      ".censorscope-innerTool{padding:2px 0 4px 0}",
      ".censorscope-row{display:flex;align-items:center;gap:8px;padding:5px 10px;margin:2px 0;",
      "border-radius:8px;border:0.5px solid var(--dsw-alias-border-l4,rgba(0,0,0,.08));",
      "background:var(--dsw-alias-bg-layer-3,#ffffff);cursor:pointer;user-select:none;min-width:0}",
      ".censorscope-row:hover{background:var(--dsw-alias-interactive-bg-hover,rgba(0,0,0,.05))}",
      ".censorscope-rowOpen{background:var(--dsw-alias-interactive-bg-hover,rgba(0,0,0,.05))}",
      ".censorscope-caret{flex:none;width:12px;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:10px}",
      ".censorscope-tag{flex:none;display:inline-flex;align-items:center;height:22px;max-width:180px;padding:0 8px;",
      "border-radius:6px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;font-weight:600;",
      "color:var(--dsw-alias-state-business-primary,#2f6bff);background:var(--dsw-alias-state-business-tertiary,#e8f0ff)}",
      ".censorscope-name{font-family:var(--ds-font-family-code,ui-monospace,SFMono-Regular,Menlo,monospace)}",
      ".censorscope-args{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;",
      "font-family:var(--ds-font-family-code,ui-monospace,SFMono-Regular,Menlo,monospace);font-size:12px;",
      "color:var(--dsw-alias-label-secondary,#646a73)}",
      ".censorscope-meta{flex:none;display:flex;align-items:center;gap:10px;color:var(--dsw-alias-label-tertiary,#8f959e);font-size:12px;white-space:nowrap}",
      ".censorscope-status{padding:1px 6px;border-radius:5px;font-size:11px;font-weight:600}",
      ".censorscope-st-success{color:var(--dsw-alias-state-success-primary,#188a4d);background:var(--dsw-alias-state-success-tertiary,#e3f5ea)}",
      ".censorscope-st-error{color:var(--dsw-alias-state-error-primary,#d92d20);background:var(--dsw-alias-state-error-tertiary,#fdecec)}",
      ".censorscope-st-running{color:var(--dsw-alias-state-warn-label,#b25e09);background:var(--dsw-alias-state-warn-tertiary,#fdf1e1)}",
      ".censorscope-detail{margin:2px 0 8px 24px;padding:8px 10px;border-radius:8px;overflow:hidden;",
      "border:0.5px solid var(--dsw-alias-border-l2,rgba(0,0,0,.1));",
      "background:var(--dsw-alias-bg-layer-2,#f7f8fa);color:var(--dsw-alias-label-primary,#1f2329)}",
      ".censorscope-chips{color:var(--dsw-alias-label-secondary,#646a73);font-size:12px;padding:2px 2px 6px;line-height:1.6}",
      ".censorscope-detailScroll{max-height:45vh;overflow:auto;border-radius:6px;border:0.5px solid var(--dsw-alias-border-l1,rgba(0,0,0,.08))}",
      ".censorscope-table{width:100%;border-collapse:collapse;font-family:var(--ds-font-family-code,ui-monospace,SFMono-Regular,Menlo,monospace);font-size:12px}",
      ".censorscope-table th{position:sticky;top:0;text-align:left;color:var(--dsw-alias-label-tertiary,#8f959e);font-weight:600;",
      "background:var(--dsw-alias-bg-layer-2,#f7f8fa);padding:4px 8px;border-bottom:1px solid var(--dsw-alias-border-l1,rgba(0,0,0,.1))}",
      ".censorscope-table td{padding:3px 8px;vertical-align:top;color:var(--dsw-alias-label-primary,#1f2329);border-bottom:0.5px solid var(--dsw-alias-border-l1,rgba(0,0,0,.05));word-break:break-all}",
      ".censorscope-table tr:last-child td{border-bottom:none}",
      ".censorscope-muted{color:var(--dsw-alias-label-tertiary,#8f959e)}",
      ".censorscope-link{cursor:pointer;color:var(--dsw-alias-state-business-primary,#2f6bff);text-decoration:underline}",
      ".censorscope-num{text-align:right}",
      ".censorscope-toolWide{margin:6px 0 2px}",
    ].join("\n");

    function ensureStyles() {
      try {
        if (document.getElementById(STYLE_ID) !== null) return;
        const node = document.createElement("style");
        node.id = STYLE_ID;
        node.textContent = STYLE_TEXT;
        document.head.appendChild(node);
      } catch {
        /* style injection is best-effort */
      }
    }

    function fmtSysTime(raw) {
      const ms = Number(raw) / 1e6;
      if (!Number.isFinite(ms)) return String(raw);
      try {
        return new Date(ms).toLocaleTimeString();
      } catch {
        return String(raw);
      }
    }

    const dash = (value) =>
      value === undefined || value === null || value === "" ? "—" : String(value);
    const th = (value) => react.createElement("th", null, value);

    function CensorScopeView(props) {
      const sessionId = (props && props.sessionId) || "";
      if (!props || !props.ledger) {
        return react.createElement("div", { className: "censorscope-root" },
          react.createElement("div", { className: "censorscope-empty" }, "CensorScope 账本不可用（会话事件源未就绪）。"));
      }
      const events = useSyncExternalStore(props.ledger.subscribe, props.ledger.getRaw);
      const toolRows = useSyncExternalStore(props.ledger.subscribe, props.ledger.getSnapshot);
      const [open, setOpen] = useState({});
      const [noiseOn, setNoiseOn] = useState({});
      const [states, setStates] = useState({});
      async function loadSyscalls(callId) {
        if (states[callId]) return;
        setStates((s) => ({ ...s, [callId]: { loading: true } }));
        try {
          const res = await fetch(
            "/censorscope/call?session=" + encodeURIComponent(sessionId) + "&call=" + encodeURIComponent(callId),
            { headers: { accept: "application/json" } },
          );
          const body = await res.json();
          setStates((s) => ({
            ...s,
            [callId]: body && body.ok ? body : { error: (body && body.error) || "HTTP " + res.status },
          }));
        } catch (error) {
          setStates((s) => ({ ...s, [callId]: { error: String((error && error.message) || error) } }));
        }
      }
      function toggle(callId) {
        if (!states[callId]) loadSyscalls(callId);
        setOpen((o) => ({ ...o, [callId]: !o[callId] }));
      }

      function syscallDetail(callId) {
        const state = states[callId];
        if (!state || state.loading) {
          return react.createElement("div", { className: "censorscope-chips" }, "系统调用加载中…");
        }
        if (state.error) {
          return react.createElement("div", { className: "censorscope-chips" }, "系统调用获取失败：" + state.error);
        }
        const all = Array.isArray(state.events) ? state.events : [];
        if (all.length === 0) {
          return react.createElement("div", { className: "censorscope-chips" }, "（该调用未采集到系统调用事件）");
        }
        // Hide fd-channel chatter (no file object, e.g. worker RPC sockets) so real file traffic stands out.
        const kept = [];
        const noise = [];
        for (const event of all) {
          if (isFdChannelNoise(event)) noise.push(event);
          else kept.push(event);
        }
        const counts = {};
        for (const event of kept) {
          const key = event.kind_name || "unknown";
          counts[key] = (counts[key] || 0) + 1;
        }
        const chips = Object.keys(counts).map((key) => key + "×" + counts[key]).join(" · ");
        const expandNoise = !!noiseOn[callId];
        const rowsSource = expandNoise ? kept.concat(noise) : kept;
        const shown = rowsSource.slice(0, 300);
        const rowEl = (event, index) =>
          react.createElement("tr", {
            key: String(event.event_id || "e") + ":" + index,
            className: isFdChannelNoise(event) ? "censorscope-muted" : null,
          },
            react.createElement("td", null, fmtSysTime(event.observed_at)),
            react.createElement("td", { className: "censorscope-num" },
              dash(event.host_pid !== undefined && event.host_pid !== null ? event.host_pid : event.process_id)),
            react.createElement("td", null,
              String(event.kind_name || "") + (event.operation && event.operation !== event.kind_name ? "/" + event.operation : "")),
            react.createElement("td", null, dash(event.path || event.endpoint || event.stream ||
              (event.fd !== undefined && event.fd !== null && event.fd !== "" ? "fd " + event.fd : null))),
            react.createElement("td", { className: "censorscope-num" },
              event.size !== undefined && event.size !== null && Number(event.size) > 0 ? dash(event.size) : "—"),
            react.createElement("td", { className: "censorscope-num" },
              event.result !== undefined && event.result !== null && event.result !== "" ? dash(event.result) : "—"),
          );
        const bodyRows = shown.map(rowEl);
        const tail = [];
        if (!expandNoise && noise.length > 0) {
          tail.push(react.createElement("tr", { key: "noise" },
            react.createElement("td", { colSpan: 6, className: "censorscope-muted" },
              react.createElement("span", null,
                "已折叠 " + noise.length + " 条 fd 通道读写（无文件对象，如 worker↔host RPC 帧）"),
              " · ",
              react.createElement("span", { className: "censorscope-link", onClick: () => setNoiseOn((n) => ({ ...n, [callId]: true })) }, "展开"),
            )));
        } else if (expandNoise) {
          if (noise.length > shown.length - kept.slice(0, 300).length) {
            tail.push(react.createElement("tr", { key: "noise-more" },
              react.createElement("td", { colSpan: 6, className: "censorscope-muted" },
                "… 另有 " + (noise.length - Math.max(0, 300 - kept.length)) + " 条 fd 通道行未显示")));
          }
          tail.push(react.createElement("tr", { key: "noise-hide" },
            react.createElement("td", { colSpan: 6, className: "censorscope-muted" },
              react.createElement("span", { className: "censorscope-link", onClick: () => setNoiseOn((n) => ({ ...n, [callId]: false })) }, "收起通道行"))));
        }
        if (rowsSource.length > shown.length) {
          tail.push(react.createElement("tr", { key: "more" },
            react.createElement("td", { colSpan: 6, className: "censorscope-muted" },
              "… 共 " + rowsSource.length + " 条，仅显示前 300 条")));
        }
        return react.createElement("div", null,
          react.createElement("div", { className: "censorscope-chips" },
            "系统调用 " + all.length + " 条" + (chips ? " · " + chips : "") +
              (noise.length > 0 ? " · fd 通道 " + noise.length : "")),
          react.createElement("div", { className: "censorscope-detailScroll" },
            react.createElement("table", { className: "censorscope-table" },
              react.createElement("thead", null,
                react.createElement("tr", null,
                  th("时间"), th("进程"), th("类型/操作"), th("目标"), th("大小"), th("结果"))),
              react.createElement("tbody", null, bodyRows.concat(tail)),
            )),
        );
      }

      function isFdChannelNoise(event) {
        if (!event || event.kind_name !== "file") return false;
        const op = event.operation || "";
        if (!/^(read|write|readv|writev|pread|pread64|pwrite|pwrite64)$/.test(op)) return false;
        if (typeof event.path === "string" && event.path !== "") return false;
        if (event.endpoint || event.stream) return false;
        return event.fd !== undefined && event.fd !== null && event.fd !== "";
      }

      function statusClass(status) {
        if (status === "error") return "censorscope-st-error";
        if (status === "success") return "censorscope-st-success";
        return "censorscope-st-running";
      }

      function toolRow(row) {
        if (!row) return null;
        const expanded = !!open[row.callId];
        const finished = row.durationMs !== null;
        const label = finished
          ? row.status === "error" ? "error" : "ok"
          : "运行中";
        const head = react.createElement("div", {
          className: "censorscope-row" + (expanded ? " censorscope-rowOpen" : ""),
          onClick: () => toggle(row.callId),
          role: "button",
          "aria-expanded": expanded ? "true" : "false",
          title: row.callId,
        },
          react.createElement("span", { className: "censorscope-caret" }, expanded ? "▾" : "▸"),
          react.createElement("span", { className: "censorscope-tag" },
            react.createElement("span", { className: "censorscope-name" }, row.name || "?")),
          react.createElement("span", { className: "censorscope-args" }, fmtArgs(row.argsRaw)),
          react.createElement("span", { className: "censorscope-meta" },
            react.createElement("span", { className: "censorscope-status " + statusClass(row.status) }, label),
            react.createElement("span", null, finished ? row.durationMs + " ms" : "—"),
            react.createElement("span", null, fmtTime(row.startTime)),
          ),
        );
        if (!expanded) return head;
        return react.createElement(react.Fragment, { key: row.callId },
          head,
          react.createElement("div", { className: "censorscope-detail" }, syscallDetail(row.callId)));
      }

      function assistantBody(item) {
        const nodes = [];
        const content = Array.isArray(item.content) ? item.content : [];
        for (const block of content) {
          const type = block && block.type;
          if (type === "reasoning" || type === "text") {
            nodes.push(
              react.createElement("div", { key: "b" + nodes.length },
                type === "reasoning"
                  ? react.createElement("div", { className: "censorscope-reasoning" }, summarizeText(block.text || "", 240))
                  : react.createElement("div", { className: "censorscope-text" }, summarizeText(block.text || "", 700)),
              ),
            );
          } else if (type === "tool-call") {
            const callId = block.callId;
            const row = callId && toolRows.find((r) => r.callId === callId);
            if (row) {
              nodes.push(react.createElement("div", { key: "tool:" + callId, className: "censorscope-toolWide" }, toolRow(row)));
            }
          }
        }
        if (nodes.length === 0) {
          nodes.push(react.createElement("div", { key: "empty", className: "censorscope-muted" }, "（无文本）"));
        }
        return nodes;
      }

      function itemView(item) {
        switch (item.kind) {
          case "turn":
            return react.createElement("div", { key: "turn:" + item.turn, className: "censorscope-turn" },
              react.createElement("span", null, "第 " + item.turn + " 轮"),
              item.model
                ? react.createElement("span", { className: "censorscope-turnMeta" },
                    react.createElement("span", null, item.model.model || ""),
                    react.createElement("span", null, item.model.provider || ""))
                : null,
              item.time ? react.createElement("span", { className: "censorscope-turnMeta" },
                  react.createElement("span", null, fmtTime(item.time))) : null,
              react.createElement("span", { className: "censorscope-turnLine" }));
          case "user":
            return react.createElement("div", { key: "user:" + item.time + ":" + String(item.text).length, className: "censorscope-block censorscope-user" },
              react.createElement("div", { className: "censorscope-blockHead" },
                react.createElement("span", { className: "censorscope-tagUser" }, "用户"),
                react.createElement("span", { className: "censorscope-usage" }, fmtTime(item.time))),
              react.createElement("div", { className: "censorscope-text" }, item.text));
          case "assistant":
            return react.createElement("div", { key: "assistant:" + item.time + ":" + String(item.textBlocks.length), className: "censorscope-block censorscope-assistant" },
              react.createElement("div", { className: "censorscope-blockHead" },
                react.createElement("span", { className: "censorscope-tagAssistant" }, "助手"),
                item.step !== undefined && item.step !== null
                  ? react.createElement("span", null, "步骤 " + item.step)
                  : null,
                item.usage ? react.createElement("span", { className: "censorscope-usage" }, item.usage) : null),
              assistantBody(item));
          case "tool":
            return react.createElement("div", { key: "tool:" + item.callId, className: "censorscope-toolWide" },
              toolRow(item.row));
          case "fallback":
            return react.createElement(react.Fragment, { key: "fallback" },
              item.rows.map((entry) => toolRow(entry.row)));
          default:
            return null;
        }
      }

      const items = buildTimeline(events, toolRows);
      const visible = items.filter((item) => item.kind !== "fallback").length;
      return react.createElement("div", { className: "censorscope-root" },
        react.createElement("div", { className: "censorscope-head" },
          react.createElement("span", { className: "censorscope-title" }, "CensorScope · 轨迹"),
          react.createElement("span", { className: "censorscope-sub" },
            toolRows.length + " 次工具调用 · " + visible + " 个事件行 · 点击工具行展开系统调用"),
        ),
        react.createElement("div", { className: "censorscope-scroll" },
          items.length === 0 || visible === 0
            ? react.createElement("div", { className: "censorscope-empty" },
                "（暂无轨迹：请先在会话中执行任务，任务期间/结束后回到本 Tab）")
            : items.map((item) => itemView(item)),
        ),
      );
    }

    function apply(ctx) {
      ensureStyles();
      const stores = new Map();
      const makeLedger = (sessionId) => {
        const cached = stores.get(sessionId);
        if (cached) return cached;
        const listeners = new Set();
        let eventsCache = null;
        let rowsCache = null;
        // Use the sanctioned SessionBinding.eventSource feed; the Session's own
        // eventsSnapshot is a private implementation detail.
        let source = null;

        const acquire = () => {
          if (source !== null) return source;
          try {
            const binding = ctx.sessions && ctx.sessions.binding
              ? ctx.sessions.binding(sessionId)
              : undefined;
            const eventSource = binding && binding.eventSource
              ? binding.eventSource
              : undefined;
            if (
              eventSource === undefined ||
              typeof eventSource.getSnapshot !== "function" ||
              typeof eventSource.subscribe !== "function"
            ) {
              return null;
            }
            source = eventSource;
            eventSource.subscribe(rebuild);
            return source;
          } catch {
            return null;
          }
        };
        const eventList = () => {
          const src = acquire();
          if (src === null) return [];
          const window = src.getSnapshot();
          const entries = window && Array.isArray(window.entries)
            ? window.entries
            : [];
          const events = [];
          for (const entry of entries) {
            if (entry && entry.type === "event" && entry.event && typeof entry.event.type === "string") {
              events.push(entry.event);
            }
          }
          return events;
        };
        const rebuild = () => {
          eventsCache = eventList();
          rowsCache = censorscopeBuildRows(eventsCache);
          for (const listener of [...listeners]) listener();
          return rowsCache;
        };
        const onBusEvent = () => {
          if (source === null) {
            const got = acquire();
            if (got !== null) rebuild();
          }
        };
        try {
          ctx.on && ctx.on("session/event", onBusEvent);
        } catch {
          /* event bus unavailable; rely on the mount-time seed */
        }
        rebuild();
        const ledger = {
          subscribe: (listener) => {
            listeners.add(listener);
            return () => listeners.delete(listener);
          },
          getSnapshot: () => (rowsCache === null ? rebuild() : rowsCache),
          getRaw: () => (eventsCache === null ? rebuild() : eventsCache),
        };
        stores.set(sessionId, ledger);
        return ledger;
      };
      ctx.slots.inject("conversation.view", () =>
        ctx.slots.register(
          {
            name: "conversation.view",
            id: "censorscope",
            order: 15,
            label: () => "CensorScope",
            inject: (sessionId) => ({ ledger: makeLedger(sessionId), sessionId }),
          },
          CensorScopeView,
        ),
      );
    }

    exports.apply = apply;
    exports.inject = inject;
    exports.censorscopeBuildRows = censorscopeBuildRows;
    return module.exports;
  },
});
