window.__ModuleLoader__.load({
  id: '@censorfs/deepseek-harness',
  factory: (require) => {
    var module = { exports: {} }
    var exports = module.exports
    Object.defineProperty(exports, Symbol.toStringTag, { value: 'Module' })

    var React = require('react')
    var h = React.createElement
    var useState = React.useState
    var useEffect = React.useEffect

    // ── 状态折叠 ──

    function updateState(state, event) {
      var data = event.data
      var variants = Object.assign({}, state.variants)
      if (event.type === 'variant-running') {
        variants[data.variantId] = Object.assign({}, data, { status: 'running', activities: [], sysActivities: [] })
      } else if (event.type === 'variant-activity') {
        var variant = variants[data.variantId]
        if (variant !== undefined) {
          variants[data.variantId] = Object.assign({}, variant, {
            activities: (variant.activities || []).concat([data.activity]),
            lastActivity: data.activity,
          })
        }
      } else if (event.type === 'variant-sys-activity') {
        var _variant = variants[data.variantId]
        if (_variant !== undefined) {
          variants[data.variantId] = Object.assign({}, _variant, {
            sysActivities: (_variant.sysActivities || []).concat([data.activity]),
            lastSysActivity: data.activity,
          })
        }
      } else if (event.type === 'variant-validated') {
        var _validation = data.validation
        var _nextStatus =
          _validation && _validation.status === 'passed'
            ? 'prepared'
            : _validation && _validation.status === 'failed'
              ? 'validation-failed'
              : 'unvalidated'
        variants[data.variantId] = Object.assign({}, variants[data.variantId], {
          validation: _validation,
          status: _nextStatus,
        })
      } else if (event.type === 'variant-prepared') {
        var _pv = data.validation
        var _ps =
          _pv && _pv.status === 'failed'
            ? 'validation-failed'
            : _pv && _pv.status === 'unvalidated'
              ? 'unvalidated'
              : 'prepared'
        variants[data.variantId] = Object.assign({}, variants[data.variantId], data, { status: _ps })
      } else if (event.type === 'variant-failed') {
        variants[data.variantId] = Object.assign({}, variants[data.variantId], data, { status: 'failed' })
      } else if (event.type === 'variant-published') {
        variants[data.variantId] = Object.assign({}, variants[data.variantId], data, { status: 'published' })
      } else if (event.type === 'variant-aborted') {
        variants[data.variantId] = Object.assign({}, variants[data.variantId], data, { status: 'aborted' })
      } else if (event.type === 'variant-stale') {
        variants[data.variantId] = Object.assign({}, variants[data.variantId], data, { status: 'stale' })
      } else if (event.type === 'isolation-fallback') {
        var fallbackVariant = variants[data.variantId]
        if (fallbackVariant !== undefined) {
          variants[data.variantId] = Object.assign({}, fallbackVariant, { isolationFallback: data })
        }
        var isolation = state.isolation === undefined ? undefined : Object.assign({}, state.isolation, {
          effectiveLevel: 'process',
          cgroupEnabled: false,
          cgroupRoot: undefined,
          warnings: (state.isolation.warnings || []).concat([data.warning]),
        })
        return Object.assign({}, state, {
          variants: variants,
          isolation: isolation,
          isolationFallbacks: (state.isolationFallbacks || []).concat([data]),
        })
      } else if (event.type === 'ranking-ready') {
        return Object.assign({}, state, { variants: variants, ranking: data.ranking, status: 'ranked' })
      } else if (event.type === 'exploration-ended') {
        return Object.assign({}, state, { variants: variants, ended: data })
      }
      return Object.assign({}, state, { variants: variants })
    }

    // ── 事件聚合定义 ──

    var branchExploreDefinition = {
      kind: 'branch-explore',
      target: 'chat',
      match: function (event) {
        if (event.type === 'exploration-started') return { id: event.data.runId, role: 'start' }
        var updateTypes = [
          'variant-running', 'variant-activity', 'variant-sys-activity',
          'variant-validated', 'variant-prepared', 'variant-failed', 'isolation-fallback',
          'ranking-ready', 'variant-published', 'variant-aborted', 'variant-stale',
          'exploration-ended',
        ]
        if (updateTypes.indexOf(event.type) >= 0) return { id: event.data.runId, role: 'update' }
        return null
      },
      start: function (_context, match) {
        if (match.event.type !== 'exploration-started') throw new Error('Branch exploration requires a start event')
        return Object.assign({}, match.event.data, { variants: {}, ranking: [], status: 'running' })
      },
      update: function (context, match) {
        return updateState(context.state, match.event)
      },
      buildViewNode: function (context) {
        if (context.start === undefined) return null
        return {
          key: context.key,
          kind: 'branch-explore',
          id: context.id,
          target: 'chat',
          anchorSeq: context.start.event.seq,
          location: context.start.location,
          visibility: 'visible',
          data: context.state,
        }
      },
    }

    // ── 全局子代理图状态折叠 ──
    // 数据源：根会话日志中的 subagent-graph-opened（一次性起点）与
    // subagent-started / subagent-ended / subagent-activity 事件（runtime 持久化）。
    // variant 绑定信息经由 type='variant/binding' 的合成活动进入节点。

    function sgStatusFromStop(stopReason) {
      if (stopReason === 'completed') return 'completed'
      if (stopReason === 'aborted') return 'aborted'
      if (stopReason === 'refusal') return 'refusal'
      if (stopReason === 'max-tokens') return 'max-tokens'
      return 'error'
    }

    function sgTouch(nodes, sessionId) {
      if (nodes[sessionId] === undefined) {
        nodes[sessionId] = {
          sessionId: sessionId,
          activities: [],
          sysActivities: [],
          status: 'running',
        }
      }
      return nodes[sessionId]
    }

    function sgApplyEvent(state, event) {
      var data = event.data
      var nodes = Object.assign({}, state.nodes)

      if (event.type === 'subagent-graph-cleared') {
        // Execution Graph 第一版保留完整历史，忽略旧 clear marker。
        return state
      }

      if (event.type === 'subagent-started') {
        // continuable 子代理（agent-teams）会重启同一 session：
        // 保留已积累的活动/标签/探索绑定，只重置运行状态，lane 连续不断
        var prev = nodes[data.sessionId]
        nodes[data.sessionId] = Object.assign({
          activities: [],
          sysActivities: [],
          status: 'running',
        }, prev, data, {
          activities: prev === undefined ? [] : prev.activities,
          sysActivities: prev === undefined ? [] : prev.sysActivities,
          status: 'running',
          startedAt: prev === undefined ? data.startedAt : prev.startedAt,
          endedAt: undefined,
          stopReason: undefined,
          // 探索绑定（runId/variantId/runTask 来自 variant/binding 活动）跨重启保留。
          // variantId 与 runId 同规：优先保留 prev，否则回落 data（首启时 data 才有值，
          // 否则被抹成 undefined，前端 groupOf 判不出 explore 分组 → 平铺 main）。
          runId: (prev !== undefined && prev.runId) || data.runId,
          variantId: (prev !== undefined && prev.variantId) || data.variantId,
          runTask: prev !== undefined ? prev.runTask : undefined,
          label: (prev !== undefined && prev.label) || data.label,
        })
      } else if (event.type === 'subagent-ended') {
        var ended = sgTouch(nodes, data.sessionId)
        nodes[data.sessionId] = Object.assign({}, ended, {
          endedAt: data.endedAt,
          stopReason: data.stopReason,
          status: sgStatusFromStop(data.stopReason),
        })
      } else if (event.type === 'subagent-activity') {
        var target = sgTouch(nodes, data.sessionId)
        var activity = data.activity
        if (activity.type === 'variant/binding') {
          // 探索绑定：不进时间线，作为节点标记 + task lane 分组信号
          nodes[data.sessionId] = Object.assign({}, target, {
            runId: activity.runId,
            runTask: activity.runTask,
            variantId: activity.variantId,
            label: target.label || activity.preview,
          })
        } else if (data.sys === true) {
          // 系统级观测活动（eBPF/auditd 注入）→ 单独时间线
          nodes[data.sessionId] = Object.assign({}, target, {
            sysActivities: target.sysActivities.concat([activity]),
          })
        } else {
          nodes[data.sessionId] = Object.assign({}, target, {
            activities: target.activities.concat([activity]),
            label: target.label || (activity.type === 'subagent/descriptor' ? activity.preview : undefined),
          })
        }
      }

      return Object.assign({}, state, { nodes: nodes })
    }

    var subagentGraphDefinition = {
      kind: 'subagent-graph',
      target: 'subagents',
      match: function (event) {
        if (event.data === undefined || event.data === null) return null
        if (event.type === 'subagent-graph-opened') {
          return { id: event.data.rootSessionId || 'graph', role: 'start' }
        }
        if (event.data.rootSessionId === undefined) return null
        if (event.type === 'subagent-started' || event.type === 'subagent-ended' || event.type === 'subagent-activity' || event.type === 'subagent-graph-cleared') {
          return { id: event.data.rootSessionId, role: 'update' }
        }
        return null
      },
      start: function (_context, match) {
        if (match.event.type !== 'subagent-graph-opened') throw new Error('subagent-graph requires subagent-graph-opened')
        return {
          rootSessionId: match.event.data.rootSessionId,
          openedAt: match.event.data.openedAt,
          nodes: {},
        }
      },
      update: function (context, match) {
        return sgApplyEvent(context.state, match.event)
      },
      buildViewNode: function (context) {
        if (context.start === undefined) return null
        return {
          key: context.key,
          kind: 'subagent-graph',
          id: context.id,
          target: 'subagents',
          anchorSeq: context.start.event.seq,
          location: context.start.location,
          visibility: 'visible',
          data: context.state,
        }
      },
    }

    // ── 样式 ──

    var panel = {
      border: '1px solid var(--border, #d6d6d6)',
      borderRadius: 12,
      padding: 16,
      margin: '10px 0',
      background: 'var(--surface, #fff)',
    }
    var cardStyle = {
      border: '1px solid var(--border, #ddd)',
      borderRadius: 9,
      padding: 14,
      marginTop: 10,
    }
    var row = { display: 'flex', gap: '8px', alignItems: 'center', flexWrap: 'wrap' }
    var muted = { opacity: 0.65, fontSize: 12 }
    var buttonStyle = { padding: '5px 12px', borderRadius: 7, border: '1px solid #ccc', cursor: 'pointer', fontSize: 13 }
    var primaryButtonStyle = Object.assign({}, buttonStyle, {
      background: '#0969da',
      borderColor: '#0969da',
      color: '#fff',
      fontWeight: 600,
    })

    // Git Graph 分支图样式
    var graphContainer = {
      fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
      fontSize: 13,
      lineHeight: 1.8,
      userSelect: 'none',
    }
    var graphRow = {
      display: 'flex',
      alignItems: 'center',
      gap: '8px',
      padding: '4px 0',
      cursor: 'default',
      transition: 'background-color 0.15s ease',
    }
    var laneWidth = 24
    var dotSize = 12
    var dotStyle = {
      width: dotSize,
      height: dotSize,
      borderRadius: '50%',
      flexShrink: 0,
      border: '2px solid',
      boxSizing: 'border-box',
    }

    // ── 颜色方案（参考 VS Code Git Graph）──

    var STATUS_COLORS = Object.freeze({
      running: '#007acc',
      prepared: '#388a34',
      'validation-failed': '#d73a49',
      failed: '#cb2431',
      published: '#9c6f0c',
      aborted: '#6a737d',
      stale: '#e36209',
      unvalidated: '#57606a',
    })

    var BRANCH_COLORS = ['#e06c75', '#98c379', '#61afef', '#c678dd', '#56b6c2', '#d19a66']

    function getVariantColor(index) {
      return BRANCH_COLORS[index % BRANCH_COLORS.length]
    }

    function getStatusColor(status) {
      return STATUS_COLORS[status] || STATUS_COLORS.stale
    }

    // ── 活动图标 ──

    var ACTIVITY_ICONS = Object.freeze({
      'tool/execution': '\u2699',
      'tool/call': '\u2699',
      'tool/result': '\u2713',
      'assistant/message': '\u2709',
      'step/start': '\u25B6',
      'turn/end': '\u25A0',
      'sys/file-write': '\u270E',
      'sys/file-read': '\uD83D\uDCC4',
      'sys/process-create': '\u2691',
      'sys/network-connect': '\u21C4',
    })

    function activityIcon(type) {
      return ACTIVITY_ICONS[type] || '\u2022'
    }

    // 对象安全序列化：turn/end 等事件的 reason/preview 可能是对象，直接拼接会变成 [object Object]
    function ggStr(v, n) {
      if (v === undefined || v === null) return ''
      if (typeof v === 'object') {
        var t = v.type !== undefined ? v.type : v.name !== undefined ? v.name : v.reason !== undefined ? v.reason : v.status
        if (t !== undefined) return ggStr(t, n)
        try { return JSON.stringify(v).slice(0, n) } catch (e) { return '[object]' }
      }
      return String(v).slice(0, n)
    }

    function activityLabel(act) {
      var type = String(act.type || '')

      if (type === 'tool/execution') {
        var xLabel = act.tool ? String(act.tool) : 'tool'

        if (act.target) {
          xLabel += ' · ' + ggStr(act.target, 90)
        }

        if (act.ok === false) {
          xLabel += '  ✕'
        } else {
          xLabel += '  ✓'
        }

        return xLabel
      }

      if (type === 'tool/call') {
        var label = act.tool ? String(act.tool) : 'tool'

        if (act.target) {
          label += ' · ' + ggStr(act.target, 90)
        }

        return label
      }

      if (type === 'tool/result') {
        var result = act.ok === false ? 'result ✕' : 'result ✓'

        if (act.tool) {
          result += ' · ' + ggStr(act.tool, 40)
        }

        if (act.preview) {
          result += ' · ' + ggStr(act.preview, 90).replace(/\s+/g, ' ')
        }

        return result
      }

      if (type === 'assistant/message') {
        return 'assistant · ' + ggStr(act.preview, 110).replace(/\s+/g, ' ')
      }

      if (type === 'step/start') {
        return 'step ' + ggStr(act.step, 30)
      }

      if (type === 'turn/end') {
        return 'turn/end · ' + ggStr(act.reason, 60)
      }

      if (act.path) {
        return type + ' · ' + act.path + (act.bytes !== undefined ? ' · ' + act.bytes + ' bytes' : '')
      }

      if (act.command) {
        return type + ' · ' + ggStr(act.command, 100)
      }

      return type || 'event'
    }

    // 单个 Activity 的详情文本（二级展开用）：
    // target/cwd/command/exit/duration/call + arguments/result/usage
    function activityDetailText(act) {
      var lines = []

      if (act.type === 'tool/execution' && act.tool) {
        lines.push('tool     ' + String(act.tool))
      }

      if (act.target) {
        lines.push('target   ' + String(act.target))
      }

      if (act.cwd) {
        lines.push('cwd      ' + String(act.cwd))
      }

      if (act.command) {
        lines.push('command  ' + String(act.command))
      }

      if (act.exitCode !== undefined) {
        lines.push('exit     ' + String(act.exitCode))
      }

      if (act.durationMs !== undefined) {
        lines.push('duration ' + String(act.durationMs) + ' ms')
      } else if (
        act.startedAt !== undefined &&
        act.endedAt !== undefined
      ) {
        lines.push(
          'duration ' +
          String(Math.max(0, act.endedAt - act.startedAt)) +
          ' ms',
        )
      }

      if (act.callId) {
        lines.push('call     ' + String(act.callId))
      }

      if (act.argsPreview) {
        if (lines.length > 0) lines.push('')
        lines.push('arguments')
        lines.push(String(act.argsPreview))
      }

      if (act.preview) {
        if (lines.length > 0) lines.push('')
        lines.push(act.type === 'assistant/message' ? 'message' : 'result')
        lines.push(String(act.preview))
      }

      if (act.usagePreview) {
        if (lines.length > 0) lines.push('')
        lines.push('usage')
        lines.push(String(act.usagePreview))
      }

      return lines.join('\n')
    }

    // 将 tool/call + tool/result 按 callId 合并成一条展示事件（tool/execution）。
    // callId 缺失（V1 旧数据）或 result 未到达（进行中）时保持原样两条。
    function buildDisplayActivities(activities) {
      var source = (activities || []).slice().sort(function (a, b) {
        return (a.at || 0) - (b.at || 0)
      })

      var resultByCall = new Map()
      source.forEach(function (act) {
        if (act.type === 'tool/result' && act.callId) {
          resultByCall.set(act.callId, act)
        }
      })

      var usedResults = new Set()
      var output = []

      source.forEach(function (act) {
        if (act.type === 'tool/result' && act.callId && usedResults.has(act.callId)) {
          return
        }

        if (act.type === 'tool/call' && act.callId) {
          var result = resultByCall.get(act.callId)

          if (result !== undefined) {
            usedResults.add(act.callId)

            output.push({
              type: 'tool/execution',
              tool: act.tool,
              target: act.target,
              command: act.command,
              cwd: act.cwd,
              argsPreview: act.argsPreview,
              callId: act.callId,
              ok: result.ok,
              exitCode: result.exitCode,
              durationMs: result.durationMs,
              preview: result.preview,
              at: act.at,
              startedAt: act.at,
              endedAt: result.at,
            })

            return
          }
        }

        output.push(act)
      })

      return output
    }

    // 超长 group label（EXPLORE task 等）单行截短，hover title 保留全文
    function compactGroupLabel(value, maxLength) {
      var text = String(value || '').replace(/\s+/g, ' ').trim()

      if (text.length <= maxLength) {
        return text
      }

      return text.slice(0, maxLength - 1) + '…'
    }

    // ── 状态文字 ──

    function statusText(status) {
      var texts = {
        running: '\u8FD0\u884C\u4E2D',
        prepared: '\u5C31\u7EEA',
        'validation-failed': '\u9A8C\u8BC1\u5931\u8D25',
        failed: '\u6267\u884C\u5931\u8D25',
        published: '\u5DF2\u53D1\u5E03\u81F3 main',
        aborted: '\u5DF3\u653E\u5F03',
        stale: 'Head \u5DF2\u53D8\u5316',
        unvalidated: '\u672A\u9A8C\u8BC1',
      }
      return texts[status] || status
    }

    function statusShort(status) {
      var shorts = {
        running: '\u25CF',
        prepared: '\u2713',
        'validation-failed': '\u2717',
        failed: '\u2717',
        published: '\u25C6',
        aborted: '\u2014',
        stale: '?',
        unvalidated: '\u25CB',
      }
      return shorts[status] || '?'
    }

    // \u2500\u2500 World Card \u8F85\u52A9 \u2500\u2500

    // \u957F ID\uFF08candidate/generation\uFF09\u622A\u77ED\uFF0Chover title \u4FDD\u7559\u5168\u6587\uFF0C\u4E0D\u62A2\u89C6\u89C9
    function shortId(value) {
      var text = String(value || '')
      return text.length > 10 ? text.slice(0, 8) + '\u2026' : text
    }

    // 把 worker output（message blocks 数组）折叠成纯文本，供 World Card Result 块渲染。
    function outputTextClient(blocks) {
      if (typeof blocks === 'string') return blocks
      if (!Array.isArray(blocks)) return ''
      return blocks
        .map(function (block) { return block && typeof block.text === 'string' ? block.text : '' })
        .join('')
    }

// World Card \u72B6\u6001\u7528\u5927\u5199\u82F1\u6587\u77ED\u6807\u7B7E\uFF08READY / PUBLISHED \u2026\uFF09\uFF0C\u4E0E\u8BBE\u8BA1\u56FE\u4E00\u81F4
    function statusLabelEn(status) {
      var labels = {
        running: 'RUNNING',
        prepared: 'READY',
        'validation-failed': 'FAILED VALIDATION',
        failed: 'FAILED',
        published: 'PUBLISHED',
        aborted: 'ABORTED',
        stale: 'STALE',
        unvalidated: 'UNVALIDATED',
      }
      return labels[status] || String(status).toUpperCase()
    }

    // ── 隔离模式徽标：根据 state.mode 显示 FUSE / in-process，与后端写入的 exploration-started.mode 对齐
    function modeBadge(state) {
      var inProcess = state.mode === 'in-process'
      return h('span', {
        style: {
          fontSize: 10,
          fontWeight: 600,
          padding: '1px 6px',
          borderRadius: 10,
          color: inProcess ? '#1a7f37' : '#6f42c1',
          background: inProcess ? '#dafbe1' : '#f5f0ff',
          border: '1px solid ' + (inProcess ? '#a5d6b2' : '#d8c5f5'),
        },
      }, inProcess ? 'IN-PROCESS' : 'FUSE ISOLATED')
    }

    // ── 分支连接线组件 ──

    function BranchConnector(_ref) {
      var isLast = _ref.isLast
      var color = _ref.color
      var pathD = isLast
        ? 'M' + (laneWidth / 2) + ',0 L' + (laneWidth / 2) + ',' + (dotSize / 2) + ' L' + laneWidth + ',' + (dotSize / 2)
        : 'M' + (laneWidth / 2) + ',0 L' + (laneWidth / 2) + ',' + dotSize
      return h('svg', {
        width: laneWidth,
        height: dotSize,
        style: { overflow: 'visible', display: 'block' },
      }, h('path', {
        d: pathD,
        stroke: color,
        strokeWidth: 2,
        fill: 'none',
        strokeLinecap: 'round',
      }))
    }

    // ── 版本分支图（THIS SESSION 的 Generation → Candidate → Published Generation 血统）──

    function buildVersionLineage(entries) {
      return (entries || []).map(function (entry) {
        var run = entry && entry.data
        if (!run) return null
        return {
          runId: run.runId,
          task: run.task,
          baselineGeneration: run.expectedHead && run.expectedHead.generation_id,
          candidates: Object.values(run.variants || {}).filter(function (v) {
            return v.candidate && v.candidate.candidate_id
          }).map(function (v) {
            return {
              variantId: v.variantId,
              label: v.label,
              candidateId: v.candidate.candidate_id,
              status: v.status,
              publishedGeneration: v.generationId || null,
            }
          }),
        }
      }).filter(Boolean)
    }

    function VersionBranchGraph(props) {
      var entries = props.entries || []
      var runs = buildVersionLineage(entries)
      if (runs.length === 0) {
        return h('div', { style: Object.assign({}, muted, { padding: 16, textAlign: 'center', fontStyle: 'italic' }) }, '暂无版本历史')
      }

      var mono = { fontFamily: 'ui-monospace, SFMono-Regular, Consolas, monospace' }
      var nodeBase = { width: 10, height: 10, display: 'inline-block', flexShrink: 0, boxSizing: 'border-box' }
      var genNode = function () {
        return h('span', { style: Object.assign({}, nodeBase, { borderRadius: '50%', background: '#24292e' }) })
      }
      var candNode = function (status) {
        return h('span', { style: Object.assign({}, nodeBase, { borderRadius: '50%', border: '2px solid ' + getStatusColor(status), background: 'transparent' }) })
      }
      var pubNode = function (status) {
        return h('span', { style: Object.assign({}, nodeBase, { background: getStatusColor(status), transform: 'rotate(45deg)', borderRadius: 2 }) })
      }

      var children = [
        h('div', { key: 'title', style: { fontSize: 10, fontWeight: 700, color: '#8b949e', letterSpacing: 0.4, textTransform: 'uppercase', marginBottom: 10 } }, 'THIS SESSION'),
      ]

      var prevPublishedGen = null
      runs.forEach(function (run) {
        var baseline = run.baselineGeneration
        var isContinuation = prevPublishedGen !== null && baseline === prevPublishedGen

        children.push(h('div', {
          key: 'main-' + run.runId,
          style: Object.assign({}, mono, { display: 'flex', alignItems: 'center', gap: 6, padding: '3px 0', fontSize: 12 }),
        },
          isContinuation ? h('span', { style: { color: '#d0d7de', fontSize: 10, flexShrink: 0 } }, '↳') : null,
          genNode(),
          h('span', { style: { color: '#24292e' } }, 'gen/' + shortId(baseline || '?')),
          h('span', { style: { color: '#24292e', fontWeight: 700 } }, 'MAIN'),
          run.task ? h('span', { style: { color: '#8b949e', fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', minWidth: 0 } }, run.task) : null,
        ))

        run.candidates.forEach(function (c, ci) {
          var isLast = ci === run.candidates.length - 1
          var statusColor = getStatusColor(c.status)
          var isPublished = c.publishedGeneration !== null && c.publishedGeneration !== undefined
          children.push(h('div', {
            key: 'cand-' + run.runId + '-' + c.variantId,
            style: Object.assign({}, mono, { display: 'flex', alignItems: 'center', gap: 6, padding: '2px 0 2px 18px', fontSize: 12 }),
          },
            h('span', { style: { color: '#d0d7de', flexShrink: 0 } }, isLast ? '└─' : '├─'),
            isPublished ? pubNode(c.status) : candNode(c.status),
            h('span', { style: { color: '#57606a' } }, 'cand/' + shortId(c.candidateId || '?')),
            h('span', { style: { color: '#24292e' } }, 'World ' + c.variantId),
            h('span', { style: { color: statusColor, fontWeight: 700, fontSize: 10 } }, statusLabelEn(c.status)),
          ))
          if (isPublished) {
            children.push(h('div', {
              key: 'pub-' + run.runId + '-' + c.variantId,
              style: Object.assign({}, mono, { display: 'flex', alignItems: 'center', gap: 6, padding: '0 0 2px 26px', fontSize: 11, color: '#8b949e' }),
            }, '▼ → gen/' + shortId(c.publishedGeneration || '?')))
          }
        })

        var published = run.candidates.find(function (c) { return c.publishedGeneration !== null && c.publishedGeneration !== undefined })
        prevPublishedGen = published ? published.publishedGeneration : prevPublishedGen
      })

      return h('div', { style: { fontSize: 12 } }, children)
    }

    // ── 活动时间线组件 ──

    function ActivityTimeline(_ref) {
      var activities = _ref.activities
      var sysActivities = _ref.sysActivities
      var maxItems = _ref.maxItems || 50

      var allActivities = []
      ;(activities || []).forEach(function (act) { allActivities.push(Object.assign({}, act, { _sys: false })) })
      ;(sysActivities || []).forEach(function (act) { allActivities.push(Object.assign({}, act, { _sys: true })) })
      allActivities.sort(function (a, b) { return (a.at || 0) - (b.at || 0) })

      var shown = allActivities.slice(-maxItems)

      if (shown.length === 0) {
        return h('div', { style: Object.assign({}, muted, { padding: '16px', textAlign: 'center', fontStyle: 'italic' }) },
          '\u6682\u65E0\u6D3B\u52A8\u8BB0\u5F55'
        )
      }

      return h('div', { style: { maxHeight: 300, overflowY: 'auto', fontSize: 12 } },
        shown.map(function (act, i) {
          return h('div', {
            key: i,
            style: { display: 'flex', gap: '6px', padding: '3px 0', borderBottom: i < shown.length - 1 ? '1px solid #f0f0f0' : 'none', alignItems: 'flex-start' },
          },
            h('span', { style: Object.assign({}, muted, { whiteSpace: 'nowrap', minWidth: 70 }) },
              new Date(act.at).toLocaleTimeString()),
            h('span', { style: { color: act._sys ? '#fd7e14' : '#495057', fontSize: 14, flexShrink: 0 } }, activityIcon(act.type)),
            h('span', { style: { flex: 1, minWidth: 0, wordBreak: 'break-word' } }, activityLabel(act)),
          )
        }),
      )
    }

    // ── Tab 样式 ──

    var tabContainer = {
      display: 'flex',
      gap: 2,
      borderBottom: '1px solid #e1e4e8',
      marginBottom: 10,
      marginTop: 10,
    }
    var tabStyle = function (active) {
      return {
        padding: '6px 12px',
        cursor: 'pointer',
        border: 'none',
        background: active ? '#f6f8fa' : 'transparent',
        borderBottom: active ? '2px solid #0366d6' : '2px solid transparent',
        color: active ? '#24292e' : '#586069',
        fontSize: 13,
        fontWeight: active ? 600 : 400,
        transition: 'all 0.15s ease',
      }
    }
    var tabContent = { padding: '8px 0', minHeight: 60 }

    // ── Parallel Worlds Dashboard 组件（第一轮 UI 改造）──
    // LifecycleStrip / WorldCard / DiffPatch 三个纯前端组件，
    // 只吃现有 foldExploration 投影出的 state，不加任何后端字段。

    var stripCellBase = {
      flex: 1,
      minWidth: 0,
      padding: '9px 12px',
      border: '1px solid #e1e4e8',
      borderRadius: 8,
      background: '#fafbfc',
    }

    function LifecycleStrip(props) {
      var state = props.state
      var variants = Object.values(state.variants)
      var total = variants.length
      var runningCount = variants.filter(function (v) { return v.status === 'running' }).length
      var createdCount = variants.filter(function (v) { return v.candidate !== undefined }).length
      var readyCount = variants.filter(function (v) { return v.status === 'prepared' }).length
      var published = variants.find(function (v) { return v.status === 'published' })
      var baseGen = state.expectedHead && state.expectedHead.generation_id

      var arrow = h('span', {
        style: { alignSelf: 'center', color: '#8b949e', fontSize: 15, flexShrink: 0, padding: '0 4px' },
      }, '\u2192')

      function cell(title, lines, accent) {
        return h('div', {
          style: Object.assign({}, stripCellBase, accent ? { borderColor: accent, background: '#fff' } : {}),
        },
          h('div', {
            style: { fontSize: 10, fontWeight: 700, letterSpacing: 0.5, color: '#8b949e', textTransform: 'uppercase' },
          }, title),
          lines.map(function (ln, i) {
            return h('div', {
              key: i,
              style: { fontSize: 12, color: '#24292e', marginTop: 2, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' },
            }, ln)
          })
        )
      }

      var worldsLines = [total + ' total']
      if (runningCount > 0) worldsLines.push(runningCount + ' running')

      var candLines = [readyCount + ' ready']

      if (createdCount !== readyCount) {
        candLines.push(createdCount + ' created')
      }

      // EVALUATION：汇总各 World 的 validation.checks
      var checkTotal = 0
      var checkPassed = 0
      var requiredTotal = 0
      var requiredPassed = 0
      variants.forEach(function (v) {
        var checks = (v.validation && v.validation.checks) || []
        checks.forEach(function (check) {
          checkTotal += 1
          if (check.passed) checkPassed += 1
          if (check.required) {
            requiredTotal += 1
            if (check.passed) requiredPassed += 1
          }
        })
      })
      var hasEvaluation = checkTotal > 0
      var evalAccent = hasEvaluation ? (requiredPassed === requiredTotal ? '#22863a' : '#cb2431') : undefined
      var evalLines = hasEvaluation
        ? [checkPassed + '/' + checkTotal + ' checks', requiredPassed + '/' + requiredTotal + ' required']
        : ['—']

      var publishLines
      var publishAccent
      if (published) {
        var pubGen = published.generationId || (published.generation && published.generation.generation_id)
        publishLines = ['Published', 'gen/' + shortId(pubGen || '?')]
        publishAccent = '#22863a'
      } else {
        publishLines = ['Waiting']
      }

      return h('div', { style: { display: 'flex', gap: 0, marginTop: 4, alignItems: 'stretch' } },
        cell('BASELINE', ['main', 'gen/' + shortId(baseGen || '?')]),
        arrow,
        cell('WORLDS', worldsLines),
        arrow,
        cell('CANDIDATES', candLines),
        arrow,
        cell('EVALUATION', evalLines, evalAccent),
        arrow,
        cell('MAIN', publishLines, publishAccent),
      )
    }

    var worldCardStyle = {
      border: '1px solid #e1e4e8',
      borderRadius: 10,
      padding: 14,
      background: '#fff',
      display: 'flex',
      flexDirection: 'column',
    }
    var infoBoxTitle = {
      fontSize: 10,
      fontWeight: 600,
      color: '#8b949e',
      textTransform: 'uppercase',
      letterSpacing: 0.4,
    }

    // 从 activities + sysActivities 中挑出「有意义的实时动作」（读/写/删/命令/工具），
    // 过滤掉 step/start、turn/end、variant/binding、subagent/descriptor 这类技术噪音，只留最近 3 条。
    function meaningfulActivities(variant) {
      var activities = []
        .concat(variant.activities || [])
        .concat(variant.sysActivities || [])

      return activities
        .filter(function (activity) {
          var type = String(activity.type || '')

          if (
            type === 'step/start' ||
            type === 'turn/end' ||
            type === 'variant/binding' ||
            type === 'subagent/descriptor'
          ) {
            return false
          }

          var cls = sgActivityClass(activity)

          return (
            cls === 'read' ||
            cls === 'write' ||
            cls === 'delete' ||
            cls === 'exec' ||
            cls === 'tool'
          )
        })
        .slice(-3)
    }

    // 可折叠长文本：默认折叠成两行，点「展开」显示全文，「收起」恢复两行
    function collapsibleLongText(text, open, onToggle) {
      var collapsedStyle = {
        display: '-webkit-box',
        WebkitBoxOrient: 'vertical',
        WebkitLineClamp: 2,
        overflow: 'hidden',
      }
      var expandedStyle = { whiteSpace: 'pre-wrap', wordBreak: 'break-word' }
      return h('div', null,
        h('div', { title: open ? undefined : text, style: open ? expandedStyle : collapsedStyle }, text),
        h('button', {
          type: 'button',
          onClick: function (event) { event.stopPropagation(); onToggle() },
          style: {
            marginTop: 2,
            padding: '0',
            border: 'none',
            background: 'none',
            color: '#0969da',
            fontSize: 11,
            fontWeight: 600,
            cursor: 'pointer',
          },
        }, open ? '收起 ▴' : '展开 ▾'),
      )
    }

    function WorldCard(props) {
      var variant = props.variant
      var validationProfile = props.validationProfile
      var previewProfile = props.previewProfile
      var index = props.index
      var invoke = props.invoke
      var fsEnabled = props.fsEnabled
      var openSession = props.openSession
      var rank = props.rank

      var busyState = useState(false)
      var busy = busyState[0]
      var setBusy = busyState[1]

      var errorState = useState('')
      var error = errorState[0]
      var setError = errorState[1]

      var noticeState = useState('')
      var notice = noticeState[0]
      var setNotice = noticeState[1]

      var strategyOpenState = useState(false)
      var strategyOpen = strategyOpenState[0]
      var setStrategyOpen = strategyOpenState[1]

      var summaryOpenState = useState(false)
      var summaryOpen = summaryOpenState[0]
      var setSummaryOpen = summaryOpenState[1]

      var expandedFileState = useState(null)
      var expandedFile = expandedFileState[0]
      var setExpandedFile = expandedFileState[1]


      var action = async function (kind) {
        setBusy(true)
        setError('')
        setNotice('')
        try {
          return await invoke(kind, variant)
        } catch (caught) {
          setError(caught instanceof Error ? caught.message : String(caught))
          return null
        } finally {
          setBusy(false)
        }
      }

      var openPreview = async function () {
        var result = await action('preview')
        if (!result) return

        var text = typeof result.text === 'string' ? result.text : ''
        var match = /https?:\/\/[^\s]+/u.exec(text)
        if (match === null) {
          // 无 Preview profile：Candidate 已就绪为只读视图（可查看文件），
          // 但没有可打开的 web 预览地址。给中性提示而不是错误。
          setNotice('Candidate 已就绪（只读视图），无 web 预览地址，可直接点击下方文件查看差异')
          return
        }
        var url = match[0].replace(/[),.;]+$/u, '')
        window.open(url, '_blank', 'noopener,noreferrer')
      }

      var canDecide = fsEnabled && ['prepared', 'validation-failed', 'unvalidated', 'stale'].indexOf(variant.status) >= 0
      var paths = variant.pathDiff || []
      var files = (variant.textDiff && variant.textDiff.files) || []
      var letter = String.fromCharCode(65 + (index || 0))
      var statusColor = getStatusColor(variant.status)
      var isRunning = variant.status === 'running'
      var candidateId = variant.candidate && variant.candidate.candidate_id
      var genId = (variant.generation && variant.generation.generation_id) || variant.generationId
      var childSessionId = variant.childSessionId
      // output 降级：后端通常折叠到 result.output；老事件仍可能直接给 variant.output。
      var resultOutput = (variant.result && variant.result.output) || variant.output
      // summary 降级：后端折叠到 result.summary；老事件仍是顶层 variant.summary。
      var summaryText = (variant.result && variant.result.summary) || variant.summary

      var actCount = (variant.activities || []).length
      var writeCount = (variant.activities || []).concat(variant.sysActivities || []).filter(function (a) {
        var c = sgActivityClass(a)
        return c === 'write' || c === 'delete'
      }).length

      var children = []

      // 头部：字母色块 + World id + 状态 + 耗时
      children.push(h('div', { key: 'header', style: { display: 'flex', alignItems: 'center', gap: 8 } },
        h('span', {
          style: {
            display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
            width: 22, height: 22, borderRadius: 6, background: statusColor, color: '#fff',
            fontWeight: 700, fontSize: 13, flexShrink: 0,
            animation: isRunning ? 'pulse 1.5s infinite' : undefined,
          },
        }, letter),
        h('strong', { style: { fontSize: 14, color: '#24292e' } }, 'World ' + variant.variantId),
        rank !== undefined
          ? h('span', { style: { fontFamily: 'ui-monospace, monospace', fontSize: 10, fontWeight: 700, background: '#f0f0f0', color: '#586069', padding: '1px 6px', borderRadius: 4 } }, '#' + rank)
          : null,
        h('span', {
          style: { marginLeft: 'auto', fontSize: 11, fontWeight: 700, letterSpacing: 0.4, color: statusColor },
        }, statusLabelEn(variant.status)),
        variant.durationMs !== undefined
          ? h('span', { style: { fontSize: 11, color: '#8b949e', fontFamily: 'ui-monospace, monospace' } }, (variant.durationMs / 1000).toFixed(1) + 's')
          : null,
      ))

      // label / strategy / summary 降级副标题
      if (variant.label && variant.label !== variant.variantId) {
        children.push(h('div', { key: 'label', style: { marginTop: 6, fontSize: 12, color: '#6f42c1', fontWeight: 600 } }, variant.label))
      }
      if (variant.strategy) {
        children.push(h('div', { key: 'strategy', style: { marginTop: 2, fontSize: 12, color: '#57606a', lineHeight: 1.5 } },
          collapsibleLongText(variant.strategy, strategyOpen, function () { setStrategyOpen(!strategyOpen) }),
        ))
      }
      // summary 与下方 RESULT 块同源（均为 worker output 的文本折叠）：
      // RESULT 块在时不再重复显示，仅老数据（无 output）时降级展示。
      if (summaryText && !(resultOutput && resultOutput.length > 0)) {
        children.push(h('div', { key: 'summary', style: { marginTop: 4, fontSize: 12, color: '#57606a', lineHeight: 1.5 } },
          collapsibleLongText(summaryText, summaryOpen, function () { setSummaryOpen(!summaryOpen) }),
        ))
      }

      // 实时「执行动态」：最近 3 条有意义的动作（读/写/删/命令/工具），实时随事件变化
      var recentActivities = meaningfulActivities(variant)
      if (recentActivities.length > 0) {
        children.push(h('div', {
          key: 'activity',
          style: {
            marginTop: 9,
            padding: '8px 9px',
            border: '1px solid #eaecef',
            borderRadius: 7,
            background: '#fafbfc',
          },
        },
          h('div', {
            style: {
              display: 'flex',
              alignItems: 'center',
              marginBottom: 5,
              fontSize: 10,
              fontWeight: 700,
              color: '#8b949e',
              letterSpacing: 0.4,
              textTransform: 'uppercase',
            },
          },
            isRunning ? '正在执行' : '最近执行',
            isRunning
              ? h('span', {
                  style: {
                    marginLeft: 'auto',
                    color: '#0969da',
                    fontSize: 9,
                  },
                }, '● LIVE')
              : null,
          ),
          recentActivities.map(function (activity, activityIndex) {
            return h('div', {
              key: String(activity.at || activityIndex) + ':' + activityIndex,
              title: activityDetailText(activity),
              style: {
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                padding: '2px 0',
                minWidth: 0,
                fontSize: 11,
                color: '#57606a',
              },
            },
              h('span', {
                style: {
                  width: 5,
                  height: 5,
                  borderRadius: '50%',
                  background: SG_ACT_COLORS[sgActivityClass(activity)] || '#8b949e',
                  flexShrink: 0,
                },
              }),
              h('span', {
                style: {
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                },
              }, activityLabel(activity)),
            )
          }),
        ))
      }

      // 文件变更：全部展示，点文件在卡片内联展开文本 diff
      var toggleFile = function (diff) {
        if (expandedFile === diff.path) { setExpandedFile(null); return }
        setExpandedFile(diff.path)
      }

      children.push(h('div', {
        key: 'files-head',
        style: { marginTop: 10, fontSize: 11, fontWeight: 600, color: '#8b949e', textTransform: 'uppercase', letterSpacing: 0.4 },
      }, '文件变更 ' + paths.length))
      if (paths.length > 0) {
        children.push(h('div', { key: 'file-list', style: { marginTop: 4 } },
          paths.map(function (diff) {
            var kindIcon = diff.kind === 'Added' ? '+' : diff.kind === 'Deleted' ? '-' : 'M'
            var kindColor = diff.kind === 'Added' ? '#22863a' : diff.kind === 'Deleted' ? '#cb2431' : '#6a737d'
            var isExpanded = expandedFile === diff.path
            return h('div', {
              key: diff.kind + ':' + diff.path,
              onClick: function () { toggleFile(diff) },
              style: {
                display: 'flex', gap: 6, padding: '2px 4px', fontFamily: 'ui-monospace, monospace', fontSize: 12,
                cursor: 'pointer', borderRadius: 4,
                background: isExpanded ? '#f0f6ff' : 'transparent',
              },
            },
              h('span', { style: { color: kindColor, fontWeight: 700, width: 14, flexShrink: 0 } }, kindIcon),
              h('span', { style: { overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 } }, diff.path),
              h('span', { style: { color: '#8b949e', fontSize: 10, flexShrink: 0 } }, isExpanded ? '▲' : '▼'),
            )
          }),
        ))

        if (expandedFile) {
          var expFile = files.find(function (f) { return f.path === expandedFile })
          var expandedDiff = paths.find(function (d) { return d.path === expandedFile })
          if (expFile && expFile.patch) {
            children.push(h('div', { key: 'inline-diff', style: { marginTop: 6 } },
              h(DiffPatch, { patch: expFile.patch }),
            ))
          } else if (expandedDiff && expandedDiff.kind === 'Deleted') {
            children.push(h('div', { key: 'inline-deleted', style: { marginTop: 6, padding: '8px 10px', border: '1px solid #eaecef', borderRadius: 6, background: '#fafbfc', fontSize: 12, color: '#8b949e', fontStyle: 'italic' } }, '该文件已被删除，无内容可预览'))
          } else {
            children.push(h('div', { key: 'inline-no-diff', style: { marginTop: 6, padding: '8px 10px', border: '1px solid #eaecef', borderRadius: 6, background: '#fafbfc', fontSize: 12, color: '#8b949e', fontStyle: 'italic' } }, '该文件无文本差异（可能为二进制文件）'))
          }
        }
      } else {
        children.push(h('div', { key: 'no-files', style: { marginTop: 4, fontSize: 12, color: '#8b949e', fontStyle: 'italic' } }, '暂无文件变更'))
      }

      // Candidate / Generation 信息格（ID 截短，title 保留全文）
      if (candidateId !== undefined || genId !== undefined) {
        children.push(h('div', {
          key: 'info',
          style: { display: 'flex', gap: 10, marginTop: 12, paddingTop: 10, borderTop: '1px solid #f0f2f4' },
        },
          h('div', { style: { flex: 1 } },
            h('div', { style: infoBoxTitle }, 'Candidate'),
            h('div', {
              title: candidateId || '',
              style: { fontFamily: 'ui-monospace, monospace', fontSize: 12, color: '#24292e', marginTop: 2 },
            }, shortId(candidateId) || '—'),
          ),
          h('div', { style: { flex: 1 } },
            h('div', { style: infoBoxTitle }, 'Generation'),
            h('div', {
              title: genId || '',
              style: { fontFamily: 'ui-monospace, monospace', fontSize: 12, color: '#24292e', marginTop: 2 },
            }, genId ? 'gen/' + shortId(genId) : '—'),
          ),
        ))
      }

      // 执行摘要（结果态）：steps / writes / 耗时。实时动作见上方「执行动态」区域。
      if (actCount > 0) {
        children.push(h('div', {
          key: 'exec-summary',
          style: { marginTop: 8, fontSize: 11, color: '#8b949e' },
        }, actCount + ' steps · ' + writeCount + ' writes' + (variant.durationMs !== undefined ? ' · ' + (variant.durationMs / 1000).toFixed(1) + 's' : '')))
      }

      // Validation 轻量行：✓/✗ · passed/total · profile
      var vChecks = (variant.validation && variant.validation.checks) || []
      if (vChecks.length > 0) {
        var vPassed = vChecks.filter(function (c) { return c.passed }).length
        var vOk = vPassed === vChecks.length
        children.push(h('div', {
          key: 'validation',
          style: { display: 'flex', alignItems: 'center', gap: 6, marginTop: 8, paddingTop: 8, borderTop: '1px solid #f0f2f4', fontSize: 12 },
        },
          h('span', { style: { fontWeight: 700, color: vOk ? '#22863a' : '#cb2431' } }, vOk ? '✓' : '✗'),
          h('span', { style: { color: '#57606a' } }, vPassed + '/' + vChecks.length + ' checks'),
          validationProfile ? h('span', { style: { color: '#8b949e' } }, '· ' + validationProfile) : null,
        ))
      }

      // worker 错误
      if (variant.error) {
        children.push(h('pre', {
          key: 'verr',
          style: { color: '#b42318', whiteSpace: 'pre-wrap', fontSize: 12, background: '#ffeef0', padding: 8, borderRadius: 4, marginTop: 8 },
        }, variant.error))
      }

      // RESULT：折叠到 result.summary / result.output（P1）。summary 已在前面
      // 降级副标题里展开过，这里把完整 output 以可折叠块呈现，避免抢占视觉。
      if (resultOutput && resultOutput.length > 0) {
        children.push(h('div', { key: 'result', style: { marginTop: 10 } },
          h('div', { style: { fontSize: 10, fontWeight: 700, color: '#8b949e', letterSpacing: 0.4, textTransform: 'uppercase', marginBottom: 3 } }, 'Result'),
          h('pre', {
            style: {
              maxHeight: 200,
              overflowY: 'auto',
              fontSize: 12,
              lineHeight: 1.5,
              fontFamily: 'ui-monospace, SFMono-Regular, Consolas, monospace',
              background: '#fafbfc',
              border: '1px solid #eaecef',
              borderRadius: 6,
              padding: 8,
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-word',
            },
          }, outputTextClient(resultOutput)),
        ))
      }

      // Session discoverability：弱入口，指向该 World 的真实 child session。
      // 主流程仍是 Result/Artifact/Diff/Publish；此处仅供 debug 跳转。
      if (childSessionId && typeof openSession === 'function') {
        children.push(h('button', {
          key: 'open-session',
          type: 'button',
          onClick: function () { openSession(childSessionId) },
          style: {
            marginTop: 10,
            padding: 0,
            border: 'none',
            background: 'none',
            color: '#0969da',
            fontSize: 11,
            fontWeight: 600,
            cursor: 'pointer',
            textAlign: 'left',
          },
        }, 'Open Session ' + shortId(childSessionId) + ' ↗'))
      }

      // 操作按钮：查看文件 / 预览 / 发布 / 放弃。预览由 previewProfile 驱动，发布/放弃沿用 prepared/stale gate。
      children.push(h('div', {
        key: 'actions',
        style: { display: 'flex', gap: 6, marginTop: 12, flexWrap: 'wrap' },
      },
        canDecide && previewProfile ? h('button', {
          type: 'button', style: buttonStyle, disabled: busy,
          title: '在新标签页打开只读 Candidate Preview',
          onClick: openPreview,
        }, 'Preview ↗') : null,
        canDecide ? h('button', {
          type: 'button',
          style: primaryButtonStyle,
          disabled: busy || variant.status === 'stale',
          onClick: function () { action('publish') },
        }, '发布') : null,
        canDecide ? h('button', {
          type: 'button',
          style: Object.assign({}, buttonStyle, { color: '#cb2431' }),
          disabled: busy,
          onClick: function () { action('abort') },
        }, '放弃') : null,
      ))

      if (error !== '') {
        children.push(h('p', { key: 'err', style: { color: '#b42318', marginTop: 8, fontSize: 12 } }, error))
      }

      if (notice !== '') {
        children.push(h('p', { key: 'notice', style: { color: '#57606a', marginTop: 8, fontSize: 12 } }, notice))
      }

      return h('article', {
        style: worldCardStyle,
        'data-branch-explore-variant': variant.variantId,
      }, children)
    }

    // ── 彩色 Unified Diff 渲染（只读 textDiff.files[].patch）──
    var diffPreStyle = {
      margin: 0,
      padding: 8,
      overflowX: 'auto',
      fontSize: 12,
      lineHeight: 1.5,
      fontFamily: 'ui-monospace, SFMono-Regular, Consolas, monospace',
      background: '#fafbfc',
      border: '1px solid #eaecef',
      borderRadius: 6,
    }

    function DiffPatch(_ref) {
      var patch = _ref.patch || ''

      return h('pre', { style: diffPreStyle },
        patch.split('\n').map(function (line, index) {
          var style = {}

          if (line.startsWith('+') && !line.startsWith('+++')) {
            style = { background: '#eaf7ee', color: '#1a7f37' }
          } else if (line.startsWith('-') && !line.startsWith('---')) {
            style = { background: '#ffebe9', color: '#cf222e' }
          } else if (line.startsWith('@@')) {
            style = { background: '#ddf4ff', color: '#0969da' }
          }

          return h('div', {
            key: index,
            style: Object.assign({
              minHeight: 18,
              whiteSpace: 'pre',
            }, style),
          }, line || ' ')
        })
      )
    }

    // ── Variant 对比表 ──

    var tableStyle = { width: '100%', borderCollapse: 'collapse', fontSize: 12, marginTop: 12 }
    var thStyle = {
      background: '#f6f8fa',
      padding: '8px 10px',
      textAlign: 'left',
      fontWeight: 600,
      borderBottom: '2px solid #e1e4e8',
      color: '#24292e',
      whiteSpace: 'nowrap',
    }
    var tdStyle = { padding: '7px 10px', borderBottom: '1px solid #f0f0f0', verticalAlign: 'middle' }

    function VariantComparisonTable(_ref) {
      var variants = _ref.variants
      var fsEnabled = _ref.fsEnabled

      if (variants.length < 2) return null

      return h('details', { key: 'comparison-table' },
        h('summary', { style: { cursor: 'pointer', fontWeight: 600, marginBottom: 8, color: '#0366d6' } },
          '\u5BF9\u6BD4\u8868 (' + variants.length + ' \u4E2A variant)'),
        h('table', { style: tableStyle },
          h('thead', null,
            h('tr', null,
              h('th', { style: thStyle }, 'Variant'),
              h('th', { style: thStyle }, '\u72B6\u6001'),
              h('th', { style: thStyle }, '\u53D8\u66F4'),
              h('th', { style: thStyle }, '\u8017\u65F6'),
              h('th', { style: thStyle }, '\u6D3B\u52A8'),
              h('th', { style: thStyle }, 'Sys'),
            )
          ),
          h('tbody', null,
            variants.map(function (variant) {
              var actCount = (variant.activities || []).length
              var sysCount = (variant.sysActivities || []).length
              var pathCount = (variant.pathDiff || []).length

              return h('tr', { key: variant.variantId },
                h('td', { style: Object.assign({}, tdStyle, { fontWeight: 500 }) }, variant.label || variant.variantId),
                h('td', { style: tdStyle },
                  h('span', { style: { display: 'inline-flex', alignItems: 'center', gap: 4, color: getStatusColor(variant.status), fontWeight: 500 }},
                    h('span', { style: Object.assign({}, dotStyle, { width: 8, height: 8, borderColor: getStatusColor(variant.status), background: variant.status === 'running' ? 'transparent' : getStatusColor(variant.status) })}),
                    statusText(variant.status).slice(0, 6),
                  )),
                h('td', { style: tdStyle }, pathCount > 0 ? String(pathCount) : '-'),
                h('td', { style: tdStyle }, variant.durationMs !== undefined ? (variant.durationMs / 1000).toFixed(1) + 's' : '-'),
                h('td', { style: tdStyle }, String(actCount)),
                h('td', { style: tdStyle }, String(sysCount)),
              )
            }),
          ),
        ),
      )
    }

    // ── 全局子代理图（树形泳道 + 时间流）──

    var SG_STATUS_COLORS = Object.freeze({
      running: '#007acc',
      idle: '#8b949e',
      completed: '#28a745',
      error: '#cb2431',
      aborted: '#6a737d',
      refusal: '#d73a49',
      'max-tokens': '#e36209',
    })

    function sgColor(status) {
      return SG_STATUS_COLORS[status] || SG_STATUS_COLORS.running
    }

    function sgStatusText(status) {
      var texts = {
        running: '运行中',
        idle: '待命',
        completed: '完成',
        error: '出错',
        aborted: '中止',
        refusal: '拒绝',
        'max-tokens': '截断',
      }
      return texts[status] || status
    }

    // ── 全局子代理图（纵向树：Y=执行流，X=分支深度；Git Graph 式克制视觉）──
    // lane 颜色按类型（main/explore/team/agent）而非 agent 身份；状态色给 commit 点
    var GG_X0 = 10               // main（root）lane x
    var GG_DX = 16               // 深度水平间距（fork 一次右移一格；兄弟同列）
    var GG_GRAPH_MIN_W = 68      // 拓扑列最小宽度（窄 lane 区，右侧才是信息主体）
    var GG_ROW_H = 27            // commit 行高（main / group / head 统一）
    var GG_ACTS_H = 21           // 活动汇总行高（折叠态 summary row）
    var GG_ACT_H = 21            // 活动明细行高（展开后的 event node）

    // 活动 → commit 点颜色（文件增删改查是重点，git graph 里相当于每次 commit 的文件变更）
    var FILE_WRITE_TOOLS = Object.freeze(['write', 'edit', 'create', 'mkdir', 'touch', 'apply_patch', 'str_replace'])
    var FILE_DELETE_TOOLS = Object.freeze(['delete', 'remove', 'rm', 'unlink'])
    var FILE_READ_TOOLS = Object.freeze(['read', 'cat', 'view', 'grep', 'glob', 'ls', 'list', 'find', 'search'])

    function sgActivityClass(act) {
      if (act._cls !== undefined) return act._cls
      var tool = String(act.tool || '').toLowerCase()
      var type = String(act.type || '')
      if (type === 'sys/file-write' || FILE_WRITE_TOOLS.includes(tool)) return 'write'
      if (type === 'sys/file-delete' || FILE_DELETE_TOOLS.includes(tool)) return 'delete'
      if (type === 'sys/file-read' || FILE_READ_TOOLS.includes(tool)) return 'read'
      if (type === 'sys/process-create' || tool === 'bash' || tool === 'shell' || tool === 'exec' || tool === 'pwsh' || tool === 'powershell' || (type === 'tool/execution' && act.command)) return 'exec'
      if (tool !== '') return 'tool'
      return 'evt'
    }

    var SG_ACT_COLORS = Object.freeze({
      write: '#22863a',   // 写/改文件 — 绿
      delete: '#cb2431',  // 删文件 — 红
      read: '#6f42c1',    // 读文件 — 紫
      exec: '#e36209',    // 执行命令 — 橙
      tool: '#8b949e',    // 其他工具 — 灰
      evt: '#c8d1d9',     // 普通事件 — 浅灰
    })

    function sgDuration(ms) {
      if (ms === undefined || ms < 0) return ''
      if (ms < 1000) return ms + 'ms'
      if (ms < 60000) return (ms / 1000).toFixed(1) + 's'
      return Math.floor(ms / 60000) + 'm' + Math.round((ms % 60000) / 1000) + 's'
    }

    // 状态 badge：小号边框式标签（git graph 提交列表风格）
    var GG_STATUS_EN = {
      running: 'RUNNING', idle: 'IDLE', completed: 'COMPLETED', error: 'ERROR', aborted: 'ABORTED',
      refusal: 'REFUSAL', 'max-tokens': 'TRUNCATED',
    }
    function statusBadge(status) {
      var c = sgColor(status)
      return h('span', {
        style: {
          fontSize: 8.5, fontWeight: 700, letterSpacing: 0.3, color: c,
          border: '1px solid ' + c, borderRadius: 3, padding: '0 4px',
          flexShrink: 0, lineHeight: '13px',
          background: status === 'running' ? c + '14' : 'transparent',
        },
      }, GG_STATUS_EN[status] || String(status).toUpperCase())
    }

    // 分组 pill：任务组标签（EXPLORE 紫 / TEAM 蓝），挂在头行第二行
    function groupPill(def) {
      var isExplore = def.kind === 'explore'
      var c = isExplore ? '#8250df' : '#0969da'
      return h('span', {
        style: {
          display: 'inline-flex', alignItems: 'center', gap: 3, flexShrink: 0,
          fontSize: 9, fontWeight: 600, color: c,
          background: c + '12', border: '1px solid ' + c + '40', borderRadius: 4, padding: '0 5px',
          maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        },
        title: def.label,
      },
        h('span', null, isExplore ? 'WORLDS' : 'TEAM'),
        h('span', { style: { opacity: 0.55 } }, '·'),
        h('span', null, def.label))
    }

    function SubagentGraphPanel(_ref) {
      var state = _ref.state

      var expandedState = useState(null)
      var expanded = expandedState[0]
      var setExpanded = expandedState[1]
      var tabState = useState('activity')
      var detailTab = tabState[0]
      var setDetailTab = tabState[1]

      // ── AgentTeams authoritative snapshot ──
      // AgentTeams 0.1.8 自己以 .agent-teams/team.json 为 truth source，
      // /plugins/dsh-agent-teams/state 会把磁盘 membership + live activity 一起返回。
      // 这里不猜 label，不猜启动时间，直接按 member.id === subagent.sessionId join。
      var teamsState = useState([])
      var agentTeams = teamsState[0]
      var setAgentTeams = teamsState[1]

      var teamDebugState = useState({
        loading: true,
        activeStatus: undefined,
        archivedStatus: undefined,
        activeCount: 0,
        archivedCount: 0,
        error: '',
      })
      var teamDebug = teamDebugState[0]
      var setTeamDebug = teamDebugState[1]

      useEffect(function () {
        var disposed = false
        var timer

        async function readTeams(url) {
          var response = await fetch(url, {
            cache: 'no-store',
          })

          var body = null

          try {
            body = await response.json()
          } catch (_error) {
            body = null
          }

          return {
            status: response.status,
            ok: response.ok,
            teams:
              response.ok &&
              body &&
              Array.isArray(body.teams)
                ? body.teams
                : [],
          }
        }

        async function refreshAgentTeams() {
          try {
            var results = await Promise.all([
              readTeams('/plugins/dsh-agent-teams/state'),
              readTeams('/plugins/dsh-agent-teams/state?archived=1'),
            ])

            var active = results[0].teams
            var archived = results[1].teams

            var merged = new Map()

            archived.forEach(function (team) {
              var key =
                String(team.workspace || '') +
                '\u0000' +
                String(team.teamId || '')

              merged.set(
                key,
                Object.assign(
                  { archived: true },
                  team,
                ),
              )
            })

            active.forEach(function (team) {
              var key =
                String(team.workspace || '') +
                '\u0000' +
                String(team.teamId || '')

              merged.set(
                key,
                Object.assign(
                  { archived: false },
                  team,
                ),
              )
            })

            if (!disposed) {
              setAgentTeams(
                Array.from(
                  merged.values(),
                ),
              )

              setTeamDebug({
                loading: false,
                activeStatus: results[0].status,
                archivedStatus: results[1].status,

                // 这里显示服务器 RAW 数量，不再显示过滤后数量
                activeCount: active.length,
                archivedCount: archived.length,

                error: '',
              })
            }
          } catch (error) {
            if (!disposed) {
              // 网络瞬断也不要清掉历史 execution graph
              setTeamDebug({
                loading: false,
                activeStatus: undefined,
                archivedStatus: undefined,
                activeCount: 0,
                archivedCount: 0,
                error:
                  error instanceof Error
                    ? error.message
                    : String(error),
              })
            }
          }
        }

        void refreshAgentTeams()

        timer = setInterval(function () {
          void refreshAgentTeams()
        }, 1500)

        return function () {
          disposed = true
          clearInterval(timer)
        }
      }, [])
      // 活动明细折叠：默认收起（只显示汇总行），点击展开为逐条 commit
      var openActsState = useState(null)
      var openActs = openActsState[0]
      var setOpenActs = openActsState[1]
      function toggleActs(sessionId) {
        var next = new Set(openActs || [])
        if (next.has(sessionId)) next.delete(sessionId)
        else next.add(sessionId)
        setOpenActs(next)
      }

      // 单个 Activity 的二级展开。
      var openEventsState = useState(null)
      var openEvents = openEventsState[0]
      var setOpenEvents = openEventsState[1]

      function activityEventKey(sessionId, act, index) {
        return sessionId + ':' + String(act.at || 0) + ':' + String(index)
      }

      function toggleActivityEvent(key) {
        var next = new Set(openEvents || [])
        if (next.has(key)) next.delete(key)
        else next.add(key)
        setOpenEvents(next)
      }

      // TEAM / EXPLORE 整组折叠：只做用户手动折叠，无隐式自动行为
      var collapsedGroupsState = useState(function () { return new Set() })
      var collapsedGroups = collapsedGroupsState[0]
      var setCollapsedGroups = collapsedGroupsState[1]

      function toggleGroup(groupKey) {
        var next = new Set(collapsedGroups || [])
        if (next.has(groupKey)) next.delete(groupKey)
        else next.add(groupKey)
        setCollapsedGroups(next)
      }

      // ── 合并 Session Event Projection + AgentTeams Truth ──

      var nodeMap = new Map()

      Object.values(state.nodes || {}).forEach(function (node) {
        nodeMap.set(node.sessionId, node)
      })

      agentTeams.forEach(function (team) {
        ;(team.members || []).forEach(function (member) {
          if (typeof member.id !== 'string' || member.id === '') {
            return
          }

          var existing = nodeMap.get(member.id)

          // 关键：
          // AgentTeams 只 enrichment，
          // 永远不负责创造 execution node。
          if (existing === undefined) {
            return
          }

          nodeMap.set(
            member.id,
            Object.assign(
              {},
              existing,
              {
                teamId: team.teamId,
                teamName: team.name,
                teamDescription:
                  team.description,

                teamRole:
                  member.role || '',

                teamMemberName:
                  member.name,

                teamMemberStatus:
                  member.status,

                teamActivity:
                  member.activity,

                teamProgress:
                  member.progress,

                teamCurrentTask:
                  member.currentTask,

                teamArchived:
                  team.archived === true,
              },
            ),
          )
        })
      })

      var nodes = Array.from(nodeMap.values())
        .sort(function (a, b) {
          return (a.startedAt || 0) - (b.startedAt || 0)
        })

      if (nodes.length === 0) {
        var debugText

        if (teamDebug.loading) {
          debugText = '正在读取 AgentTeams 状态…'
        } else if (teamDebug.error) {
          debugText =
            'AgentTeams 状态读取失败：' +
            teamDebug.error
        } else {
          debugText =
            '暂无子代理活动 · ' +
            'AgentTeams active HTTP ' +
            String(teamDebug.activeStatus) +
            ' / ' +
            String(teamDebug.activeCount) +
            ' teams · archived HTTP ' +
            String(teamDebug.archivedStatus) +
            ' / ' +
            String(teamDebug.archivedCount) +
            ' teams'
        }

        return h(
          'div',
          {
            className: 'subagent-graph-panel',
            style: panel,
          },

          h(
            'h4',
            {
              style: {
                margin: 0,
                fontSize: 15,
              },
            },
            '执行图'
          ),

          h(
            'div',
            {
              style: Object.assign({}, muted, {
                padding: 24,
                textAlign: 'center',
              }),
            },
            debugText
          )
        )
      }

      var now = Date.now()
      function fmtT(ms) {
        return ms === undefined ? '' : new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
      }

      // AgentTeams member.id 就是 startContinuable() 返回的 child session id。
      // 因此这是确定性 join，不是 heuristic。
      var teamByMember = new Map()

      agentTeams.forEach(function (team) {
        var teamKey =
          'team:' +
          String(team.workspace || '') +
          ':' +
          String(team.teamId || team.name || 'unknown')

        ;(team.members || []).forEach(function (member) {
          if (typeof member.id !== 'string' || member.id === '') return

          teamByMember.set(member.id, {
            groupKey: teamKey,
            teamId: team.teamId,
            teamName: team.name || team.teamId || 'Team',
            description: team.description,
            workspace: team.workspace,
            captainSessionId: team.captainSessionId,
            archived: team.archived === true,

            memberName: member.name,
            role: member.role || '',
            memberStatus: member.status,
            activity: member.activity,
            progress: member.progress,
            currentTask: member.currentTask,
            unread: member.unread,
          })
        })
      })

      function displayStatus(node) {
        var teamMember = teamByMember.get(node.sessionId)

        if (teamMember !== undefined) {
          if (teamMember.archived === true) {
            return 'completed'
          }

          if (teamMember.memberStatus === 'removed') return 'aborted'

          if (
            teamMember.activity === 'working' ||
            teamMember.memberStatus === 'working'
          ) {
            return 'running'
          }

          if (
            teamMember.activity === 'idle' ||
            teamMember.memberStatus === 'idle'
          ) {
            return 'idle'
          }
        }

        return node.status
      }

      // ── 父子关系 + 分组信号（只用真实数据，不伪造节点关系）──
      // explore run → 同 runId 的 variant 归为一条任务分支（分支再分支）
      // agent-teams → label 官方约定 agent-teams:{teamId}:{member}，同 teamId 归为同一条团队分支
      var nodeIds = new Set(nodes.map(function (n) { return n.sessionId }))
      var childrenOf = new Map()
      nodes.forEach(function (n) {
        var key = n.parentSessionId && nodeIds.has(n.parentSessionId) ? n.parentSessionId : 'main'
        if (!childrenOf.has(key)) childrenOf.set(key, [])
        childrenOf.get(key).push(n)
      })
      childrenOf.forEach(function (bucket) {
        bucket.sort(function (a, b) { return (a.startedAt || 0) - (b.startedAt || 0) })
      })

      function groupOf(n) {
        // ── AgentTeams ──
        // member.id === subagent sessionId，是确定性 membership。
        var teamMember = teamByMember.get(n.sessionId)

        if (teamMember !== undefined) {
          return {
            key: teamMember.groupKey,
            kind: 'team',
            label: teamMember.teamName,
            teamId: teamMember.teamId,
            description: teamMember.description,
            workspace: teamMember.workspace,
          }
        }

        // ── BranchExplore ──
        // 注意：DSH 普通 subagent 本身也有 runId。
        // 只有 variant/binding 写入了 variantId 的节点，
        // 才是真正的 branch_explore variant。
        if (n.variantId !== undefined && n.runId !== undefined) {
          return {
            key: 'explore:' + n.runId,
            kind: 'explore',
            label: n.runTask || 'Parallel Worlds',
          }
        }

        // ── AgentTeams legacy descriptor fallback ──
        var m = /^agent-teams:([^:]+):(.+)$/.exec(String(n.label || ''))
        if (m !== null) {
          return {
            key: 'team:legacy:' + m[1],
            kind: 'team',
            label: m[1],
          }
        }

        return null
      }

      function memberName(n) {
        var teamMember = teamByMember.get(n.sessionId)

        if (teamMember !== undefined) {
          return teamMember.memberName || 'member'
        }

        // 真正的 BranchExplore variant：优先用 label，否则回落 Agent
        // （前面的 badge 已经显示 "World <variantId>"，这里不再重复 id）
        if (n.variantId !== undefined) {
          var label = String(n.label || '')

          if (
            label !== '' &&
            label !== n.variantId
          ) {
            return label
          }

          return 'Agent'
        }

        // legacy AgentTeams descriptor
        var m = /^agent-teams:([^:]+):(.+)$/.exec(String(n.label || ''))
        if (m !== null) return m[2]

        var lab = String(n.label || '')

        // provider 自动生成的 opaque spawn id 不作为 UI 主身份
        if (lab.indexOf('spawn:') === 0) {
          return 'subagent'
        }

        if (lab !== '') {
          return lab.length > 36 ? lab.slice(0, 36) + '…' : lab
        }

        return 'subagent'
      }

      var roots = childrenOf.get('main') || []

      var groupMembers = new Map()
      roots.forEach(function (r) {
        var g = groupOf(r)
        if (g === null) return
        if (!groupMembers.has(g.key)) groupMembers.set(g.key, { def: g, members: [] })
        groupMembers.get(g.key).members.push(r)
      })
      var emittedGroups = new Set()
      var entries = [] // { kind:'group', def, members } | { kind:'agent', node, pill }
      roots.forEach(function (r) {
        var g = groupOf(r)
        if (g !== null) {
          if (!emittedGroups.has(g.key)) {
            emittedGroups.add(g.key)
            entries.push({ kind: 'group', def: g, members: groupMembers.get(g.key).members })
          }
        } else {
          entries.push({ kind: 'agent', node: r, pill: null })
        }
      })
      var teamGroupCount = 0
      var exploreGroupCount = 0
      var worldCount = 0
      groupMembers.forEach(function (entry) {
        if (entry.def.kind === 'team') teamGroupCount += 1
        if (entry.def.kind === 'explore') {
          exploreGroupCount += 1
          worldCount += entry.members.length
        }
      })

      // ── 行模型 + 纵向树 lane：Y=执行流（时间向下），X=分支深度（fork 才横移）──
      // 兄弟节点同列（树形缩进），普通事件沿本 lane 向下；颜色按 lane 类型，不按 agent
      var rows = []
      var laneDepth = new Map()  // ownerKey → 嵌套深度（决定 X）
      var laneKind = new Map()   // ownerKey → 'main'|'explore'|'team'|'agent'（决定颜色）
      var laneStartY = new Map() // ownerKey → 竖线起点 y
      var laneEndY = new Map()   // ownerKey → 竖线末端 y
      var laneRunning = new Map()
      var parentKeyOf = new Map() // sessionId → 父 lane ownerKey（fork/merge 曲线用）
      var cursorY = 0
      var lastCy = 0
      function pushRow(row) { rows.push(row); lastCy = row.y + row.h / 2 }

      // root lane：main 是整棵树的根
      laneDepth.set('main', 0)
      laneKind.set('main', 'main')
      laneStartY.set('main', GG_ROW_H / 2)
      pushRow({ kind: 'main', y: cursorY, h: GG_ROW_H })
      cursorY += GG_ROW_H

      // 显示树：main → [group|agent]；group → members；嵌套子代理挂其 parent 的显示节点
      var displayBySession = new Map()
      function displayNode(key, data) { return Object.assign({ key: key, children: [] }, data) }
      var displayRoot = displayNode('main', {})
      entries.forEach(function (entry) {
        if (entry.kind === 'group') {
          var gn = displayNode('group:' + entry.def.key, { entry: entry })
          displayRoot.children.push(gn)
          entry.members.forEach(function (m) {
            var mn = displayNode(m.sessionId, { node: m })
            gn.children.push(mn)
            displayBySession.set(m.sessionId, mn)
          })
        } else {
          var an = displayNode(entry.node.sessionId, { node: entry.node })
          displayRoot.children.push(an)
          displayBySession.set(entry.node.sessionId, an)
        }
      })
      // 嵌套子代理（成员再 spawn）：parentSessionId 指向已入树的子代理时挂其下
      nodes.forEach(function (n) {
        if (displayBySession.has(n.sessionId)) return
        var holder = displayBySession.get(n.parentSessionId)
        var dn = displayNode(n.sessionId, { node: n })
        if (holder !== undefined) holder.children.push(dn)
        else displayRoot.children.push(dn)
      })

      // 深度分配：child depth = parent depth + 1（兄弟同列，X 只表示层级深度）
      function assignDepth(dn, depth) {
        laneDepth.set(dn.key, depth)
        if (dn.entry !== undefined) laneKind.set(dn.key, dn.entry.def.kind === 'explore' ? 'explore' : 'team')
        else if (dn.key !== 'main') laneKind.set(dn.key, 'agent')
        dn.children.forEach(function (c) { assignDepth(c, depth + 1) })
      }
      assignDepth(displayRoot, 0)

      // 铺一个代理节点：head（commit）行 + 活动汇总/明细行
      function placeHead(node, parentKey, pill) {
        parentKeyOf.set(node.sessionId, parentKey)
        var headRow = { kind: 'head', node: node, pill: pill, fromKey: parentKey, y: cursorY, h: GG_ROW_H }
        pushRow(headRow)
        cursorY += GG_ROW_H
        laneStartY.set(node.sessionId, headRow.y + GG_ROW_H / 2)

        var rawActs = (node.activities || [])
          .filter(function (a) {
            // descriptor 是身份 metadata，不占用户的执行时间线（label 仍保留在 node 上）
            return a.type !== 'subagent/descriptor'
          })
          .concat(node.sysActivities || [])

        var acts = buildDisplayActivities(rawActs)

        var open = openActs !== null && openActs.has(node.sessionId)
        pushRow({ kind: 'acts', node: node, acts: acts, open: open, y: cursorY, h: GG_ACTS_H })
        cursorY += GG_ACTS_H
        if (open) {
          acts.forEach(function (a, ai) {
            var eventKey = activityEventKey(node.sessionId, a, ai)
            var detail = activityDetailText(a)
            var eventOpen = detail !== '' && openEvents !== null && openEvents.has(eventKey)
            var detailLines = eventOpen && detail !== '' ? detail.split('\n').length : 0
            var eventH = eventOpen
              ? Math.min(150, Math.max(54, 28 + detailLines * 14))
              : GG_ACT_H

            pushRow({
              kind: 'act',
              node: node,
              act: a,
              eventKey: eventKey,
              eventOpen: eventOpen,
              eventDetail: detail,
              y: cursorY,
              h: eventH,
            })

            cursorY += eventH
          })
        }
      }
      // 子树铺完后回填 lane 末端（父 lane 贯穿其子分支区域）
      function finishLane(node) {
        var running = displayStatus(node) === 'running'
        laneRunning.set(node.sessionId, running)
        laneEndY.set(node.sessionId, running ? cursorY : lastCy)
      }

      // 深度优先铺行（行序 = 树序，列号来自 assignCols）
      function placeTree(dn, parentKey) {
        if (dn.entry !== undefined) {
          var ownerKey = dn.key
          var collapsed = collapsedGroups !== null && collapsedGroups.has(dn.entry.def.key)
          var grow = { kind: 'group', def: dn.entry.def, members: dn.entry.members, collapsed: collapsed, y: cursorY, h: GG_ROW_H }
          pushRow(grow)
          laneStartY.set(ownerKey, grow.y + GG_ROW_H / 2)
          var lastCyBefore = lastCy
          lastCy = grow.y + GG_ROW_H / 2
          cursorY += GG_ROW_H

          if (!collapsed) {
            dn.children.forEach(function (c) { placeTree(c, ownerKey) })
          }

          var gRunning = !collapsed && dn.entry.members.some(function (m) { return displayStatus(m) === 'running' })
          laneRunning.set(ownerKey, gRunning)
          laneEndY.set(ownerKey, gRunning ? cursorY : (collapsed ? grow.y + GG_ROW_H / 2 : Math.max(lastCy, lastCyBefore)))
        } else {
          placeHead(dn.node, parentKey, null)
          dn.children.forEach(function (c) { placeTree(c, dn.node.sessionId) })
          finishLane(dn.node)
        }
      }
      displayRoot.children.forEach(function (dn) { placeTree(dn, 'main') })

      var totalH = cursorY
      // 颜色按 lane 类型（Git Graph 式克制）：root 近黑 / explore 紫 / team 蓝 / agent 中性灰
      var GG_KIND_COLORS = Object.freeze({ main: '#24292e', explore: '#8250df', team: '#0969da', agent: '#8b949e' })
      function laneColor(key) { return GG_KIND_COLORS[laneKind.get(key) || 'agent'] }
      function laneX(key) { return GG_X0 + (laneDepth.get(key) || 0) * GG_DX }
      var maxDepth = 0
      laneDepth.forEach(function (d) { if (d > maxDepth) maxDepth = d })
      var graphW = Math.max(GG_GRAPH_MIN_W, GG_X0 + 14 + (maxDepth + 1) * GG_DX)

      // ── 拓扑列 SVG：竖线 lane（直线为主）→ fork/merge 小曲线 → commit 节点 ──
      var graphEls = []
      var GG_CURVE = 8 // fork / merge 曲线纵向跨度（唯一允许曲线的地方）

      // main root lane：贯穿全高的竖直线
      graphEls.push(h('line', { key: 'main-line', x1: GG_X0, y1: GG_ROW_H / 2, x2: GG_X0, y2: totalH, stroke: GG_KIND_COLORS.main, strokeWidth: 2 }))

      // 各 lane 竖线（任务组 / 子代理）：fork 点 → 末端，直线；运行中虚线呼吸
      laneDepth.forEach(function (_d, key) {
        if (key === 'main') return
        var running = laneRunning.get(key) === true
        graphEls.push(h('line', {
          key: 'lane-' + key,
          x1: laneX(key), y1: laneStartY.get(key),
          x2: laneX(key), y2: laneEndY.get(key),
          stroke: laneColor(key), strokeWidth: 2,
          strokeDasharray: running ? '5,3' : 'none',
          opacity: running ? 1 : 0.8,
        }))
      })

      // fork 曲线：父 lane 在 head/group 行上方引出，落到子 lane 的 commit 点（唯一曲线之一）
      rows.forEach(function (row) {
        if (row.kind !== 'head' && row.kind !== 'group') return
        var cy = row.y + row.h / 2
        var childKey = row.kind === 'group' ? 'group:' + row.def.key : row.node.sessionId
        var fromKey = row.kind === 'group' ? 'main' : row.fromKey
        var px = laneX(fromKey)
        var cx = laneX(childKey)
        if (px === cx) return
        graphEls.push(h('path', {
          key: 'fork-' + childKey,
          d: 'M ' + px + ',' + (cy - GG_CURVE) + ' C ' + px + ',' + cy + ' ' + cx + ',' + (cy - GG_CURVE) + ' ' + cx + ',' + cy,
          stroke: laneColor(childKey), strokeWidth: 1.8, fill: 'none',
        }))
      })

      // merge 曲线：成功完成的分支并回父 lane（abort/error 只终止不合并）
      function drawMerge(childKey, fromKey) {
        var cx = laneX(childKey), px = laneX(fromKey)
        if (cx === px) return
        var ey = laneEndY.get(childKey)
        if (ey === undefined) return
        graphEls.push(h('path', {
          key: 'merge-' + childKey,
          d: 'M ' + cx + ',' + ey + ' C ' + cx + ',' + (ey + GG_CURVE) + ' ' + px + ',' + (ey - GG_CURVE) + ' ' + px + ',' + ey,
          stroke: '#8b949e', strokeWidth: 1.6, fill: 'none', opacity: 0.55,
        }))
      }
      nodes.forEach(function (node) {
        if (displayStatus(node) !== 'completed') {
          return
        }

        drawMerge(node.sessionId, parentKeyOf.get(node.sessionId) || 'main')
      })
      rows.forEach(function (row) {
        if (row.kind !== 'group') return
        var allDone = row.members.every(function (m) {
          return displayStatus(m) === 'completed'
        })
        if (!allDone) return
        drawMerge('group:' + row.def.key, 'main')
      })

      // commit 节点（最后绘制，覆盖在线上）
      rows.forEach(function (row, ri) {
        var cy = row.y + row.h / 2
        if (row.kind === 'main') {
          // root：实心点 + 外环，区别于普通 commit
          graphEls.push(h('circle', { key: 'dot-main-ring', cx: GG_X0, cy: cy, r: 6.5, fill: 'none', stroke: GG_KIND_COLORS.main, strokeWidth: 1.2, opacity: 0.45 }))
          graphEls.push(h('circle', { key: 'dot-main', cx: GG_X0, cy: cy, r: 4, fill: GG_KIND_COLORS.main }, h('title', null, 'main root')))
        } else if (row.kind === 'group') {
          // 任务组 = 大节点：类型色实心大圆 + 白描边
          var gk = 'group:' + row.def.key
          graphEls.push(h('circle', {
            key: 'dot-' + gk, cx: laneX(gk), cy: cy, r: 5.5,
            fill: laneColor(gk), stroke: '#fff', strokeWidth: 1.5,
          }, h('title', null, row.def.label)))
        } else if (row.kind === 'head') {
          // 代理启动 = commit：颜色表达状态（运行空心蓝 / 完成绿 / 失败红）
          var shownStatus = displayStatus(row.node)
          var running = shownStatus === 'running'
          var scol = sgColor(shownStatus)
          graphEls.push(h('circle', {
            key: 'dot-' + row.node.sessionId,
            cx: laneX(row.node.sessionId), cy: cy, r: 4.5,
            fill: running ? '#fff' : scol, stroke: scol, strokeWidth: 2,
            style: running ? { animation: 'pulse 1.2s infinite' } : undefined,
          }, h('title', null, memberName(row.node))))
        } else if (row.kind === 'acts') {
          // 活动汇总 = 空心小圆
          if (row.acts.length === 0) return
          graphEls.push(h('circle', {
            key: 'dot-acts-' + row.node.sessionId,
            cx: laneX(row.node.sessionId), cy: cy, r: 2.5,
            fill: '#fff', stroke: laneColor(row.node.sessionId), strokeWidth: 1.4,
          }, h('title', null, row.acts.length + ' acts')))
        } else if (row.kind === 'act') {
          // 活动事件 = 按 类型着色的小 commit
          var cls = sgActivityClass(row.act)
          var c = SG_ACT_COLORS[cls]
          graphEls.push(h('circle', {
            key: 'dot-act-' + ri,
            cx: laneX(row.node.sessionId), cy: cy,
            r: (cls === 'write' || cls === 'delete' || cls === 'read') ? 3.5 : 2.5,
            fill: cls === 'evt' ? '#fff' : c, stroke: c, strokeWidth: 1.2,
          }, h('title', null, activityLabel(row.act))))
        }
      })

      // ── 信息列（与拓扑行一一对应）──
      var infoRows = []
      rows.forEach(function (row, ri) {
        var cy = row.y + row.h / 2
        if (row.kind === 'main') {
          // main = 整个 graph 的 root，不是普通任务标题
          infoRows.push(h('div', {
            key: 'info-main',
            style: { height: row.h, display: 'flex', alignItems: 'center', gap: 7, paddingLeft: 8, borderBottom: '1px solid #f0f2f4' },
          },
            h('span', { style: { width: 11, height: 11, borderRadius: 3, background: GG_KIND_COLORS.main, flexShrink: 0 } }),
            h('span', { style: { fontSize: 13, fontWeight: 800, color: '#24292e' } }, 'main'),
            h('span', { style: { fontSize: 8.5, fontWeight: 700, letterSpacing: 0.3, color: '#57606a', border: '1px solid #d0d7de', borderRadius: 3, padding: '0 4px', flexShrink: 0 } }, 'ROOT'),
            h('span', { style: Object.assign({}, muted, { fontSize: 10, marginLeft: 'auto', flexShrink: 0, paddingLeft: 6, fontFamily: 'ui-monospace, monospace' }) }, fmtT(state.openedAt))))
        } else if (row.kind === 'group') {
          // 任务组 = 大节点行：整行可点击折叠/展开；▶/▼ + 任务名 + 类型 badge + 成员数
          var gRunning = !row.collapsed && row.members.some(function (m) { return displayStatus(m) === 'running' })
          var gStart = row.members.reduce(function (m, x) { return Math.min(m, x.startedAt || Infinity) }, Infinity)
          var displayGroupLabel = compactGroupLabel(row.def.label, row.def.kind === 'explore' ? 46 : 32)
          infoRows.push(h('div', {
            key: 'info-group-' + row.def.key,
            style: {
              height: row.h,
              display: 'flex',
              alignItems: 'center',
              gap: 6,
              paddingLeft: 8,
              background: 'rgba(0,0,0,0.03)',
              borderBottom: '1px solid #eef0f2',
              cursor: 'pointer',
              userSelect: 'none',
            },
            onClick: function () { toggleGroup(row.def.key) },
          },
            h('span', {
              style: { width: 10, flexShrink: 0, fontSize: 9, color: '#8b949e', transition: 'transform 0.15s', display: 'inline-block', transform: row.collapsed ? 'none' : 'rotate(90deg)' },
            }, '▶'),
            h('span', {
              style: { fontSize: 12, fontWeight: 700, color: laneColor('group:' + row.def.key), overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flexShrink: 1 },
              title: row.def.label,
            }, displayGroupLabel),
            h('span', {
              style: { fontSize: 8.5, fontWeight: 700, letterSpacing: 0.3, flexShrink: 0, borderRadius: 3, padding: '0 4px', color: row.def.kind === 'explore' ? '#8250df' : '#0969da', border: '1px solid ' + (row.def.kind === 'explore' ? '#8250df55' : '#0969da55') },
            }, row.def.kind === 'explore' ? 'WORLDS' : 'TEAM'),
            h('span', {
              style: Object.assign({}, muted, { fontSize: 10, flexShrink: 0 }),
            }, row.def.kind === 'explore' ? row.members.length + ' worlds' : row.members.length + ' members'),
            gRunning ? h('span', { style: { color: sgColor('running'), fontSize: 10, fontWeight: 700, flexShrink: 0 } }, '●') : null,
            h('span', { style: Object.assign({}, muted, { fontSize: 10, marginLeft: 'auto', flexShrink: 0, paddingLeft: 6, fontFamily: 'ui-monospace, monospace' }) }, fmtT(gStart === Infinity ? undefined : gStart))))
        } else if (row.kind === 'head') {
          // 代理启动 = commit row：单行紧凑；点击行展开/收起 event nodes
          var node = row.node
          var isOpen = openActs !== null && openActs.has(node.sessionId)
          var isSel = expanded === node.sessionId
          var name = memberName(node)
          var tm = teamByMember.get(node.sessionId)
          infoRows.push(h('div', {
            key: 'info-head-' + node.sessionId,
            style: {
              height: row.h, display: 'flex', alignItems: 'center', gap: 6, paddingLeft: 8,
              cursor: 'pointer', borderBottom: '1px solid #f6f8fa',
              background: isSel ? 'rgba(9,105,218,0.07)' : isOpen ? 'rgba(0,0,0,0.02)' : 'transparent',
            },
            onClick: function () { toggleActs(node.sessionId) },
          },
            node.variantId !== undefined
              ? h('span', { style: { fontSize: 9.5, fontWeight: 700, color: laneColor(node.sessionId), fontFamily: 'ui-monospace, monospace', flexShrink: 0 } }, 'World ' + node.variantId)
              : null,
            h('span', { style: { fontSize: 12, fontWeight: 600, color: '#24292e', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flexShrink: 1 }, title: name }, name),
            tm !== undefined && tm.role && tm.role !== name
              ? h('span', {
                  style: {
                    fontSize: 10,
                    color: '#8b949e',
                    flexShrink: 0,
                  },
                }, tm.role)
              : null,
            statusBadge(displayStatus(node)),
            h('span', { style: { fontSize: 10, color: '#8b949e', flexShrink: 0, fontFamily: 'ui-monospace, monospace' } },
              sgDuration(node.endedAt !== undefined ? node.endedAt - (node.startedAt || node.endedAt) : now - (node.startedAt || now))),
            row.pill ? groupPill(row.pill) : null,
            h('span', { style: { fontSize: 8.5, width: 9, color: '#8b949e', flexShrink: 0, transition: 'transform 0.15s', display: 'inline-block', transform: isOpen ? 'rotate(90deg)' : 'none' } }, '▶'),
            h('span', { style: Object.assign({}, muted, { fontSize: 10, marginLeft: 'auto', flexShrink: 0, paddingLeft: 6, fontFamily: 'ui-monospace, monospace' }) }, fmtT(node.startedAt)),
            h('button', {
              style: { background: 'none', border: 'none', color: isSel ? '#0366d6' : '#8b949e', cursor: 'pointer', fontSize: 11, padding: '0 2px', flexShrink: 0, lineHeight: 1 },
              title: 'session 详情',
              onClick: function (e) { e.stopPropagation(); setExpanded(isSel ? null : node.sessionId) },
            }, 'ⓘ')))
        } else if (row.kind === 'acts') {
          // 汇总行：折叠态显示分类计数 + 最后活动预览；点击展开/收起明细
          var nodeS = row.node
          var counts = { write: 0, delete: 0, read: 0, exec: 0, tool: 0, evt: 0 }
          row.acts.forEach(function (a) { counts[sgActivityClass(a)] = (counts[sgActivityClass(a)] || 0) + 1 })
          var lastA = row.acts.length > 0 ? row.acts[row.acts.length - 1] : undefined
          var durS = nodeS.endedAt !== undefined ? sgDuration(nodeS.endedAt - (nodeS.startedAt || nodeS.endedAt)) : sgDuration(now - (nodeS.startedAt || now))
          var countParts = []
          if (counts.write > 0) countParts.push(h('span', { key: 'cw', style: { color: SG_ACT_COLORS.write } }, counts.write + 'w'))
          if (counts.delete > 0) countParts.push(h('span', { key: 'cd', style: { color: SG_ACT_COLORS.delete } }, counts.delete + 'd'))
          if (counts.read > 0) countParts.push(h('span', { key: 'cr', style: { color: SG_ACT_COLORS.read } }, counts.read + 'r'))
          if (counts.exec > 0) countParts.push(h('span', { key: 'ce', style: { color: SG_ACT_COLORS.exec } }, counts.exec + 'x'))
          if (counts.tool > 0) countParts.push(h('span', { key: 'ct', style: { color: SG_ACT_COLORS.tool } }, counts.tool + 't'))
          infoRows.push(h('div', {
            key: 'info-acts-' + nodeS.sessionId,
            style: {
              height: row.h, display: 'flex', alignItems: 'center', gap: 5, paddingLeft: 8,
              fontSize: 10, color: '#8b949e', cursor: row.acts.length > 0 ? 'pointer' : 'default',
            },
            onClick: function () { if (row.acts.length > 0) toggleActs(nodeS.sessionId) },
          },
            row.acts.length > 0
              ? h('span', { style: { fontSize: 8, width: 9, flexShrink: 0, color: '#8b949e', transition: 'transform 0.15s', display: 'inline-block', transform: row.open ? 'rotate(90deg)' : 'none' } }, '▶')
              : null,
            row.acts.length > 0
              ? h('span', { style: { flexShrink: 0 } }, row.acts.length + ' steps')
              : h('span', { style: { fontStyle: 'italic' } }, 'no activity'),
            countParts.length > 0
              ? h('span', { style: { display: 'inline-flex', gap: 4, flexShrink: 0 } }, countParts)
              : null,
            h('span', { style: { flexShrink: 0 } }, '· ' + durS),
            !row.open && lastA !== undefined
              ? h('span', { style: { overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', minWidth: 0 } },
                  activityIcon(lastA.type) + ' ' + activityLabel(lastA))
              : null,
            lastA !== undefined
              ? h('span', { style: Object.assign({}, muted, { fontSize: 9.5, marginLeft: 'auto', flexShrink: 0, paddingLeft: 6, fontFamily: 'ui-monospace, monospace' }) }, fmtT(lastA.at))
              : null))
        } else if (row.kind === 'act') {
          var act = row.act
          var isSys = act.type !== undefined && String(act.type).indexOf('sys/') === 0
          var canExpand = row.eventDetail !== ''

          infoRows.push(
            h('div', {
              key: 'info-act-' + row.node.sessionId + '-' + ri,
              style: {
                height: row.h,
                boxSizing: 'border-box',
                display: 'flex',
                flexDirection: 'column',
                justifyContent: row.eventOpen ? 'flex-start' : 'center',
                gap: 3,
                padding: row.eventOpen ? '5px 8px' : '0 8px',
                fontSize: 11.5,
                color: isSys ? '#e36209' : '#57606a',
                borderBottom: '1px solid #f3f4f6',
                background: row.eventOpen ? '#f6f8fa' : 'transparent',
                cursor: canExpand ? 'pointer' : 'default',
              },
              onClick: canExpand
                ? function () { toggleActivityEvent(row.eventKey) }
                : undefined,
            },
              // 第一行：compact event
              h('div', {
                style: {
                  display: 'flex',
                  alignItems: 'center',
                  gap: 7,
                  minHeight: 18,
                  width: '100%',
                },
              },
                h('span', { style: { fontSize: 11, flexShrink: 0, opacity: 0.85 } }, activityIcon(act.type)),
                h('span', {
                  style: {
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                    minWidth: 0,
                  },
                }, activityLabel(act)),
                canExpand
                  ? h('span', { style: { fontSize: 8, opacity: 0.55, flexShrink: 0 } }, row.eventOpen ? '▼' : '▶')
                  : null,
                h('span', { style: Object.assign({}, muted, {
                  fontSize: 10,
                  marginLeft: 'auto',
                  flexShrink: 0,
                  paddingLeft: 6,
                  fontFamily: 'ui-monospace, monospace',
                }) }, fmtT(act.at)),
              ),
              // 第二层：payload
              row.eventOpen
                ? h('pre', {
                    style: {
                      margin: '2px 0 0 18px',
                      padding: '6px 8px',
                      maxHeight: Math.max(34, row.h - 30),
                      overflow: 'auto',
                      borderRadius: 5,
                      background: '#ffffff',
                      border: '1px solid #eaecef',
                      fontSize: 10.5,
                      lineHeight: 1.45,
                      fontFamily: 'ui-monospace, SFMono-Regular, Consolas, monospace',
                      whiteSpace: 'pre-wrap',
                      wordBreak: 'break-word',
                      color: '#24292f',
                    },
                  }, row.eventDetail)
                : null,
            ),
          )
        }
      })

      // ── 详情面板（README：Tab 化，含 session 轨迹）──
      var detailPanel = null
      if (expanded !== null) {
        var enode = state.nodes[expanded]
        if (enode !== undefined) {
          var tab = detailTab
          var setTab = setDetailTab
          var toolActs = enode.activities || []
          var sysActs = enode.sysActivities || []
          var durE = enode.endedAt !== undefined ? sgDuration(enode.endedAt - (enode.startedAt || enode.endedAt)) : sgDuration(now - (enode.startedAt || now))

          detailPanel = h('div', {
            key: 'detail',
            style: { marginTop: 10, background: '#fff', borderRadius: 8, padding: 14, border: '1px solid #d8dee4' },
          },
            h('div', { style: { display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: 4 } },
              h('div', null,
                h('div', { style: { fontSize: 14, fontWeight: 700, color: '#24292e' } },
                  (enode.label || (enode.provider || 'subagent') + ':' + String(enode.sessionId).slice(0, 8)),
                  enode.variantId !== undefined ? h('span', { style: { background: '#fff3cd', color: '#946c00', fontSize: 10, padding: '1px 6px', borderRadius: 4, marginLeft: 8, verticalAlign: 'middle' } }, 'explore/' + enode.variantId) : null,
                  enode.runId !== undefined ? h('span', { style: { color: '#9333ea', fontSize: 10, marginLeft: 6, verticalAlign: 'middle' } }, enode.runId) : null),
                h('div', { style: Object.assign({}, muted, { fontSize: 11, marginTop: 3, fontFamily: 'ui-monospace, monospace', wordBreak: 'break-all' }) },
                  'session: ', h('code', null, enode.sessionId),
                  enode.parentSessionId !== undefined ? h('span', null, ' · parent: ' + String(enode.parentSessionId).slice(0, 12)) : null,
                  ' · provider: ' + (enode.provider || '?'),
                  ' · ', fmtT(enode.startedAt), enode.endedAt !== undefined ? ' → ' + fmtT(enode.endedAt) + ' (' + durE + ')' : ' (运行 ' + durE + ')'),
                enode.stopReason !== undefined ? h('div', { style: { fontSize: 11, color: '#cb2431', marginTop: 2 } }, 'stop reason: ' + enode.stopReason) : null),
              h('div', { style: { display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 } },
                h('span', { style: { color: sgColor(displayStatus(enode)), fontWeight: 700, fontSize: 12 } }, sgStatusText(displayStatus(enode))),
                h('button', {
                  style: { background: 'none', border: '1px solid #ddd', borderRadius: 5, padding: '2px 9px', fontSize: 11, cursor: 'pointer', color: '#666' },
                  onClick: function () { setExpanded(null) },
                }, '关闭'))),
            h('div', { style: { display: 'flex', gap: 14, fontSize: 11, color: '#586069', padding: '6px 0', borderBottom: '1px solid #eef0f2', marginBottom: 6 } },
              h('span', null, '工具活动: ', h('b', { style: { color: '#333' } }, toolActs.length)),
              h('span', null, '系统级: ', h('b', { style: { color: '#e36209' } }, sysActs.length)),
              h('span', null, '文件操作: ', h('b', { style: { color: '#22863a' } },
                toolActs.concat(sysActs).filter(function (a) {
                  var c = sgActivityClass(a); return c === 'write' || c === 'delete' || c === 'read'
                }).length))),
            h('div', { style: tabContainer },
              h('button', { style: tabStyle(tab === 'activity'), onClick: function () { setTab('activity') } }, '活动轨迹 (' + toolActs.length + ')'),
              h('button', { style: tabStyle(tab === 'sys'), onClick: function () { setTab('sys') } }, '系统级 (' + sysActs.length + ')')),
            h('div', { style: tabContent },
              tab === 'activity'
                ? h(ActivityTimeline, { activities: toolActs, sysActivities: [], maxItems: 200 })
                : h(ActivityTimeline, { activities: [], sysActivities: sysActs, maxItems: 200 })),
          )
        }
      }

      var runningCount = nodes.filter(function (n) { return displayStatus(n) === 'running' }).length
      var fileOps = 0
      nodes.forEach(function (n) {
        ;(n.activities || []).concat(n.sysActivities || []).forEach(function (a) {
          var cls = sgActivityClass(a)
          if (cls === 'write' || cls === 'delete' || cls === 'read') fileOps++
        })
      })

      return h('div', { className: 'subagent-graph-panel', style: panel },
        h('style', null, `
          @keyframes pulse {
            0%, 100% { opacity: 1; }
            50% { opacity: 0.45; }
          }
        `),
        h('h4', { style: { margin: '0 0 8px 0', fontSize: 15, display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' } },
          '执行图',
          h('span', { style: Object.assign({}, muted, { fontSize: 12, fontWeight: 400 }) },
            nodes.length + ' 子代理 · ' +
            (teamGroupCount > 0 ? 'TEAM ' + teamGroupCount + ' · ' : '') +
            (worldCount > 0 ? 'WORLDS ' + worldCount + ' · ' : '') +
            runningCount + ' 运行中 · ' + fileOps + ' 次文件操作'),
          h('span', { style: { display: 'inline-flex', gap: 9, fontSize: 10, color: '#666', marginLeft: 'auto', alignItems: 'center', flexShrink: 0 } },
            h('span', { style: { display: 'inline-flex', alignItems: 'center', gap: 3 } }, h('span', { style: { width: 7, height: 7, borderRadius: '50%', background: SG_ACT_COLORS.write, display: 'inline-block' } }), '写/改'),
            h('span', { style: { display: 'inline-flex', alignItems: 'center', gap: 3 } }, h('span', { style: { width: 7, height: 7, borderRadius: '50%', background: SG_ACT_COLORS.delete, display: 'inline-block' } }), '删'),
            h('span', { style: { display: 'inline-flex', alignItems: 'center', gap: 3 } }, h('span', { style: { width: 7, height: 7, borderRadius: '50%', background: SG_ACT_COLORS.read, display: 'inline-block' } }), '读'),
            h('span', { style: { display: 'inline-flex', alignItems: 'center', gap: 3 } }, h('span', { style: { width: 7, height: 7, borderRadius: '50%', background: SG_ACT_COLORS.exec, display: 'inline-block' } }), '命令'),
            h('span', { style: { display: 'inline-flex', alignItems: 'center', gap: 3 } }, h('span', { style: { width: 7, height: 7, borderRadius: '50%', background: SG_ACT_COLORS.tool, display: 'inline-block' } }), '工具')),
        ),
        // git graph 主体：左拓扑列（SVG）+ 右信息列，逐行对齐，时间纵向流动
        h('div', { style: { display: 'flex', border: '1px solid #eaecef', borderRadius: 8, overflow: 'hidden', background: '#fff' } },
          h('div', { key: 'graph-col', style: { width: graphW, flexShrink: 0, borderRight: '1px solid #eaecef', background: '#fafbfc', position: 'relative' } },
            h('svg', {
              key: 'graph-svg',
              width: graphW,
              height: totalH,
              viewBox: '0 0 ' + graphW + ' ' + totalH,
              style: { display: 'block' },
            }, ...graphEls)),
          h('div', { key: 'info-col', style: { flex: 1, minWidth: 0 } }, ...infoRows),
        ),
        detailPanel,
      )
    }

    // ── 主面板 ──

function BranchExplorePanel(_ref) {
      var state = _ref.state
      var invokeAction = _ref.invoke
      var openSession = _ref.openSession
      var variants = Object.values(state.variants)
      var rankById = new Map((state.ranking || []).map(function (entry) { return [entry.variantId, entry.rank] }))

      // 打开面板时向服务端拉一次权威状态：state() 读路径的 reconcile 会把
      // 重启残留的假 running 补写成终态，补写事件经订阅推回，旧会话显示自愈。
      var runId = state.runId
      useEffect(function () {
        if (runId === undefined) return
        invokeAction('state', { runId: runId }).catch(function () {})
      }, [runId])

      var busyState = useState(false)
      var busy = busyState[0]
      var setBusy = busyState[1]

      var errorState = useState('')
      var error = errorState[0]
      var setError = errorState[1]

      var invoke = async function (action, variant) {
        setBusy(true)
        setError('')
        try {
          return await invokeAction(action, { runId: state.runId, variantId: variant.variantId })
        } catch (caught) {
          setError(caught instanceof Error ? caught.message : String(caught))
        } finally {
          setBusy(false)
        }
      }

      var worldGridStyle = {
        display: 'flex',
        flexDirection: 'column',
        gap: 12,
        marginTop: 14,
      }

      return h('div', { className: 'branch-explore-panel', style: panel },
        // pulse 动画：BranchGraph 不再渲染，需在此注入 keyframes（World Card 头部 running 态用）
        h('style', null, `
          @keyframes pulse {
            0%, 100% { opacity: 1; transform: scale(1); }
            50% { opacity: 0.6; transform: scale(0.92); }
          }
        `),
h('h4', { style: { margin: '0 0 6px 0', fontSize: 15, display: 'flex', alignItems: 'center', gap: 8 } },
          'Parallel Worlds',
          modeBadge(state),
          busy ? h('span', { style: { fontSize: 12, color: '#007acc' } }, '处理中...') : null,
        ),

        // 任务描述
        state.task ? h('div', {
          title: state.task,
          style: {
            fontSize: 12, color: '#57606a', lineHeight: 1.5, marginBottom: 6,
            display: '-webkit-box', WebkitBoxOrient: 'vertical', WebkitLineClamp: 2, overflow: 'hidden',
          },
        }, state.task) : null,

        // 生命周期条（Baseline → Worlds → Candidates → Publish）
        h(LifecycleStrip, { state: state }),

        // World Cards 单列纵向
        h('div', { style: worldGridStyle },
          variants.map(function (variant, index) {
            return h(WorldCard, {
              key: variant.variantId,
              variant: variant,
              index: index,
              invoke: invoke,
              fsEnabled: state.fsEnabled,
              rank: rankById.get(variant.variantId),
              validationProfile: state.validationProfile,
              previewProfile: state.previewProfile,
              openSession: openSession,
            })
          }),
        ),

        error ? h('div', { style: { color: '#b42318', marginTop: 8, padding: 8, background: '#ffeef0', borderRadius: 4 } }, error) : null,

        // 结束信息
        state.ended ? h('div', { style: Object.assign({}, muted, { marginTop: 12, paddingTop: 8, borderTop: '1px solid #e1e4e8', fontSize: 12 }) },
          '探索结束: ' + (state.ended.reason || 'completed') +
          (state.ended.elapsedMs ? ' (耗时 ' + (state.ended.elapsedMs / 1000).toFixed(1) + 's)' : ''),
        ) : null,
      )
    }


    // ── 聊天卡片包装（conversation.chat.node 槽位组件）──

    // action → host slash command，再解包 RemoteResult。聊天卡与 Worlds 视图共用。
    function makeWorldInvoke(executeCommand) {
      return async function (action, payload) {
        var command
        if (action === 'preview') command = '/censorfs-preview ' + payload.runId + ' ' + payload.variantId
        else if (action === 'publish') command = '/censorfs-publish ' + payload.runId + ' ' + payload.variantId
        else if (action === 'abort') command = '/censorfs-abort ' + payload.runId + ' ' + payload.variantId
        else if (action === 'validate') command = '/censorfs-validate ' + payload.runId + ' ' + payload.variantId
        else if (action === 'state') command = '/censorfs-state ' + payload.runId
        else throw new Error('unsupported action: ' + action)

        // ctx.remote.commands.execute 返回 RemoteResult：
        //   { ok:true, value:{ commandId, result:{ kind:'success'|'error', text } } }
        //   { ok:false, error:{ code, message } }  (传输失败)
        var execution = await executeCommand(command)
        if (!execution || !execution.ok) {
          throw new Error(execution && execution.error
            ? execution.error.code + ': ' + execution.error.message
            : 'command failed')
        }
        if (execution.value === undefined) {
          throw new Error('unknown or malformed command: ' + command)
        }
        if (execution.value.result.kind === 'error') {
          throw new Error(execution.value.result.text)
        }
        return execution.value.result
      }
    }

    // ── 聊天卡：降级为只读摘要（完整 Dashboard 在 Worlds 标签）──
    function BranchExploreSummary(props) {
      var state = props.state
      var variants = Object.values(state.variants)
      var total = variants.length
      var runningCount = variants.filter(function (v) { return v.status === 'running' }).length
      var readyCount = variants.filter(function (v) { return v.status === 'prepared' }).length
      var published = variants.find(function (v) { return v.status === 'published' })

return h('div', { className: 'branch-explore-summary', style: panel },
        h('div', { style: { display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' } },
          h('strong', { style: { fontSize: 14 } }, 'Parallel Worlds'),
          modeBadge(state),
          published ? h('span', { style: { fontSize: 11, color: '#22863a', fontWeight: 700, marginLeft: 'auto' } }, 'PUBLISHED') : null,
        ),
        h('div', { style: Object.assign({}, muted, { fontSize: 12, marginTop: 6, display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }) },
          h('span', null, total + ' Worlds · ' + runningCount + ' Running · ' + readyCount + ' Ready'),
          h('span', null, '·'),
          variants.map(function (variant) {
            var sc = getStatusColor(variant.status)
            return h('span', { key: variant.variantId, style: { display: 'inline-flex', alignItems: 'center', gap: 3 } },
              h('span', { style: { width: 7, height: 7, borderRadius: '50%', background: sc, display: 'inline-block' } }),
              variant.variantId + ':' + statusLabelEn(variant.status))
          }),
        ),
        h('div', { style: Object.assign({}, muted, { fontSize: 11, marginTop: 8, paddingTop: 6, borderTop: '1px solid #f0f2f4' }) },
          '完整 Dashboard 见顶部 ', h('b', null, 'Worlds'), ' 标签'),
      )
    }

    // ── 聊天卡片包装（conversation.chat.node 槽位组件）── 只读摘要
    function BranchExploreCard(_ref) {
      var node = _ref.node
      return h(BranchExploreSummary, { state: node.data })
    }

    // ── 全局子代理图：会话顶部固定视图 tab ──

    // views target 'subagents' 的快照 builder：
    // runtime 把 target='subagents' 的聚合节点送进来，我们维护最新的节点集
    var SubagentViewBuilder = class {
      nodes = new Map()
      empty = { nodes: [] }
      replace(input) {
        this.nodes.clear()
        for (const node of input.nodes) this.nodes.set(node.key, node)
        return this.snapshot()
      }
      apply(input) {
        for (const node of input.upserts) this.nodes.set(node.key, node)
        return this.snapshot()
      }
      snapshot() {
        return { nodes: Array.from(this.nodes.values()) }
      }
    }

    var subagentViewDefinition = {
      target: 'subagents',
      create: () => new SubagentViewBuilder(),
    }

    // 视图 tab 内容：从会话快照读 views.get('subagents') 渲染图。
    // 注意：useSession 的快照是 Session 生命周期状态（无 views 字段），
    // 会话视图快照必须走 ui-conversation 注入的 useConversation（ConversationSnapshot）。
    function SubagentView(props) {
      var useConversation = props.useConversation

      var view = useConversation(function (snapshot) {
        return snapshot.views.get('subagents')
      })

      var entries = (view && view.nodes) || []
      var entry = entries[0]

      // 重要：
      // 即使持久化 subagent projection 暂时为空，也必须 mount GraphPanel。
      // GraphPanel 还需要从 AgentTeams authoritative /state 补 membership。
      var graphState =
        entry !== undefined && entry.data !== undefined
          ? entry.data
          : {
              rootSessionId: undefined,
              openedAt: Date.now(),
              nodes: {},
            }

      return h(
        'div',
        {
          style: {
            height: '100%',
            overflowY: 'auto',
            padding: 12,
            boxSizing: 'border-box',
          },
        },
        h(SubagentGraphPanel, {
          state: graphState,
        })
      )
    }

    // ── Worlds 顶级视图：同一批 exploration/variant 事件再投影一份到 'worlds' target ──
    // 不改 runtime/event schema，仅在前端多注册一份聚合 → 独立全宽 Dashboard。
    var worldsEventDefinition = {
      kind: 'branch-explore-worlds',
      target: 'worlds',
      match: branchExploreDefinition.match,
      start: branchExploreDefinition.start,
      update: function (context, match) {
        return updateState(context.state, match.event)
      },
      buildViewNode: function (context) {
        if (context.start === undefined) return null
        return {
          key: context.key,
          kind: 'branch-explore-worlds',
          id: context.id,
          target: 'worlds',
          anchorSeq: context.start.event.seq,
          location: context.start.location,
          visibility: 'visible',
          data: context.state,
        }
      },
    }

    var WorldsViewBuilder = class {
      nodes = new Map()
      empty = { nodes: [] }
      replace(input) {
        this.nodes.clear()
        for (const node of input.nodes) this.nodes.set(node.key, node)
        return this.snapshot()
      }
      apply(input) {
        for (const node of input.upserts) this.nodes.set(node.key, node)
        return this.snapshot()
      }
      snapshot() {
        return { nodes: Array.from(this.nodes.values()) }
      }
    }

    var worldsViewDefinition = {
      target: 'worlds',
      create: () => new WorldsViewBuilder(),
    }

    // 大屏两列布局：主 Dashboard（flex）+ 执行图右栏（仅 min-width:1180px 显示）
    var worldsLayoutCss = `
      .worlds-layout { display: flex; flex-direction: column; height: 100%; box-sizing: border-box; }
      .worlds-exec { display: none; }
      @media (min-width: 1180px) {
        .worlds-layout { flex-direction: row; }
        .worlds-main { flex: 1 1 auto; min-width: 0; }
        .worlds-exec { display: flex; flex-direction: column; flex: 0 0 clamp(500px, 31vw, 620px); min-width: 500px; overflow-x: auto; }
      }
    `

    // ── Worlds 执行图：按 variant 分组的紧凑执行面板（替代全局 SubagentGraphPanel）──
    function WorldExecutionPanel(_ref) {
      var state = _ref.state

      var nodeList = Object.values(state.nodes || {}).sort(function (a, b) {
        return (a.startedAt || 0) - (b.startedAt || 0)
      })

      var byVariant = new Map()
      var variantOrder = []
      var mainNodes = []
      nodeList.forEach(function (node) {
        if (node.variantId !== undefined) {
          if (!byVariant.has(node.variantId)) {
            byVariant.set(node.variantId, [])
            variantOrder.push(node.variantId)
          }
          byVariant.get(node.variantId).push(node)
        } else {
          mainNodes.push(node)
        }
      })

      var now = Date.now()

      function nodeCounts(node) {
        var counts = { write: 0, delete: 0, read: 0, exec: 0, tool: 0, total: 0 }
        ;(node.activities || []).concat(node.sysActivities || []).forEach(function (a) {
          var cls = sgActivityClass(a)
          if (counts[cls] !== undefined) counts[cls] += 1
          counts.total += 1
        })
        return counts
      }

      function nodeDur(node) {
        return sgDuration(node.endedAt !== undefined ? node.endedAt - (node.startedAt || node.endedAt) : now - (node.startedAt || now))
      }

      function countPills(counts) {
        var parts = []
        if (counts.write > 0) parts.push(h('span', { key: 'w', style: { color: SG_ACT_COLORS.write } }, counts.write + 'w'))
        if (counts.delete > 0) parts.push(h('span', { key: 'd', style: { color: SG_ACT_COLORS.delete } }, counts.delete + 'd'))
        if (counts.read > 0) parts.push(h('span', { key: 'r', style: { color: SG_ACT_COLORS.read } }, counts.read + 'r'))
        if (counts.exec > 0) parts.push(h('span', { key: 'e', style: { color: SG_ACT_COLORS.exec } }, counts.exec + 'x'))
        if (counts.tool > 0) parts.push(h('span', { key: 't', style: { color: SG_ACT_COLORS.tool } }, counts.tool + 't'))
        return parts.length > 0 ? h('span', { style: { display: 'inline-flex', gap: 4 } }, parts) : null
      }

      function agentRow(node) {
        var counts = nodeCounts(node)
        var name = String(node.label || '')
        if (name === '' || name.indexOf('spawn:') === 0) name = 'subagent'
        if (name.length > 36) name = name.slice(0, 36) + '…'
        return h('div', {
          key: node.sessionId,
          style: { display: 'flex', alignItems: 'center', gap: 6, padding: '5px 0', borderBottom: '1px solid #f3f4f6', fontSize: 12 },
        },
          h('span', { style: { width: 8, height: 8, borderRadius: '50%', background: sgColor(node.status || 'running'), flexShrink: 0 } }),
          h('span', { style: { fontWeight: 600, color: '#24292e', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flexShrink: 1 }, title: String(node.label || '') }, name),
          statusBadge(node.status || 'running'),
          h('span', { style: { fontSize: 10, color: '#8b949e', fontFamily: 'ui-monospace, monospace', flexShrink: 0 } }, nodeDur(node)),
          h('span', { style: { marginLeft: 'auto', display: 'inline-flex', gap: 5, fontSize: 10, fontWeight: 600, flexShrink: 0 } }, counts.total, countPills(counts)),
        )
      }

      function variantBlock(variantId, nodes, index) {
        var letter = String.fromCharCode(65 + (index || 0))
        var counting = { write: 0, delete: 0, read: 0, exec: 0, tool: 0, total: 0 }
        var running = false
        var startedAt = Infinity
        var endedAt
        var label = ''
        nodes.forEach(function (n) {
          var c = nodeCounts(n)
          counting.write += c.write; counting.delete += c.delete; counting.read += c.read
          counting.exec += c.exec; counting.tool += c.tool; counting.total += c.total
          if (n.status === 'running') running = true
          if (n.startedAt !== undefined) startedAt = Math.min(startedAt, n.startedAt)
          if (n.endedAt !== undefined) endedAt = Math.max(endedAt, n.endedAt)
          if (label === '' && n.label) label = String(n.label)
        })
        var dur = startedAt === Infinity ? '' : sgDuration((endedAt !== undefined ? endedAt : now) - startedAt)

        return h('div', { key: variantId, style: { marginBottom: 10, border: '1px solid #eaecef', borderRadius: 8, overflow: 'hidden' } },
          h('div', { style: { display: 'flex', alignItems: 'center', gap: 7, padding: '7px 10px', background: '#f6f8fa', borderBottom: '1px solid #eaecef' } },
            h('span', { style: { display: 'inline-flex', alignItems: 'center', justifyContent: 'center', width: 18, height: 18, borderRadius: 5, background: getVariantColor(index), color: '#fff', fontWeight: 700, fontSize: 11, flexShrink: 0 } }, letter),
            h('span', { style: { fontWeight: 700, fontSize: 12, color: '#24292e', fontFamily: 'ui-monospace, monospace', flexShrink: 0 } }, 'World ' + variantId),
            h('span', { style: { flexShrink: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 11, color: '#57606a' }, title: label }, label),
            running ? h('span', { style: { color: sgColor('running'), fontSize: 10, fontWeight: 700, flexShrink: 0 } }, '●') : null,
            h('span', { style: { marginLeft: 'auto', fontSize: 10, color: '#8b949e', fontFamily: 'ui-monospace, monospace', flexShrink: 0 } }, dur),
          ),
          h('div', { style: { padding: '4px 10px 8px' } },
            nodes.map(agentRow),
          ),
        )
      }

      if (nodeList.length === 0) {
        return h('div', { className: 'world-execution-panel', style: panel },
          h('h4', { style: { margin: 0, fontSize: 15 } }, '执行图'),
          h('div', { style: Object.assign({}, muted, { padding: 24, textAlign: 'center' }) }, '暂无子代理活动 · 运行一次探索后显示各 World 的执行轨迹'))
      }

      return h('div', { className: 'world-execution-panel', style: panel },
        h('h4', { style: { margin: '0 0 8px 0', fontSize: 15, display: 'flex', alignItems: 'center', gap: 8 } },
          '执行图',
          h('span', { style: Object.assign({}, muted, { fontSize: 12, fontWeight: 400 }) },
            variantOrder.length + ' Worlds · ' + mainNodes.length + ' main · ' + nodeList.length + ' 子代理'),
        ),
        variantOrder.map(function (variantId, index) {
          return variantBlock(variantId, byVariant.get(variantId), index)
        }),
        mainNodes.length > 0
          ? h('div', { style: { border: '1px solid #eaecef', borderRadius: 8, overflow: 'hidden' } },
              h('div', { style: { display: 'flex', alignItems: 'center', gap: 7, padding: '7px 10px', background: '#f6f8fa', borderBottom: '1px solid #eaecef' } },
                h('span', { style: { display: 'inline-flex', alignItems: 'center', justifyContent: 'center', width: 18, height: 18, borderRadius: 5, background: '#24292e', color: '#fff', fontWeight: 700, fontSize: 11 } }, 'M'),
                h('span', { style: { fontWeight: 700, fontSize: 12, color: '#24292e' } }, 'main')),
              h('div', { style: { padding: '4px 10px 8px' } }, mainNodes.map(agentRow)),
            )
          : null,
      )
    }

    function WorldsView(props) {
      // useSession 的快照是 Session 生命周期状态（无 views 字段）；
      // 会话视图快照走 ui-conversation 注入的 useConversation（ConversationSnapshot.views）。
      var useConversation = props.useConversation
      var rawExec = props.executeCommand
      var executeCommand = typeof rawExec === 'function'
        ? rawExec
        : function () { return Promise.reject(new Error('no active session for this view')) }

      var worldsView = useConversation(function (snapshot) {
        return snapshot.views.get('worlds')
      })
      var subagentsView = useConversation(function (snapshot) {
        return snapshot.views.get('subagents')
      })

      var entries = ((worldsView && worldsView.nodes) || [])
        .slice()
        .sort(function (a, b) {
          return (a.anchorSeq || 0) - (b.anchorSeq || 0)
        })

      var latestEntry = entries[entries.length - 1]
      var latestRunId =
        latestEntry &&
        latestEntry.data &&
        latestEntry.data.runId

      var selectedRunState = useState(null)
      var selectedRunId = selectedRunState[0]
      var setSelectedRunId = selectedRunState[1]

      var rightTabState = useState('exec')
      var rightTab = rightTabState[0]
      var setRightTab = rightTabState[1]

      // 新 Explore 出现时，自动切到最新 Run。
      // 用户手动查看旧 Run 时，只要没有新的 Explore，就不会把他强行切回来。
      useEffect(function () {
        if (latestRunId) {
          setSelectedRunId(latestRunId)
        }
      }, [latestRunId])

      var entry = entries.find(function (item) {
        return (
          item.data &&
          item.data.runId === selectedRunId
        )
      }) || latestEntry

      var state =
        entry !== undefined &&
        entry.data !== undefined
          ? entry.data
          : null

      var invoke = makeWorldInvoke(executeCommand)

      var runSelector =
        entries.length > 1 && state
          ? h('div', {
              style: {
                display: 'flex',
                alignItems: 'center',
                gap: 8,
                padding: '8px 16px 0',
              },
            },
              h('span', {
                style: {
                  fontSize: 11,
                  fontWeight: 600,
                  color: '#8b949e',
                },
              }, 'RUN'),

              h('select', {
                value: state.runId || '',
                onChange: function (event) {
                  setSelectedRunId(event.target.value)
                },
                style: {
                  minWidth: 240,
                  maxWidth: 520,
                  height: 30,
                  padding: '0 28px 0 9px',
                  border: '1px solid #d0d7de',
                  borderRadius: 6,
                  background: '#fff',
                  color: '#24292e',
                  fontSize: 12,
                },
              },
                entries
                  .slice()
                  .reverse()
                  .map(function (item, index) {
                    var run = item.data || {}
                    var task = String(
                      run.task || 'Parallel Worlds'
                    )
                      .replace(/\s+/g, ' ')
                      .trim()

                    var label =
                      (index === 0 ? 'Latest · ' : '') +
                      (task.length > 55
                        ? task.slice(0, 54) + '…'
                        : task)

                    return h('option', {
                      key: run.runId || item.key,
                      value: run.runId || '',
                    }, label)
                  }),
              ),
            )
          : null

      // 执行图右栏：复用 subagents 投影（GraphPanel 自带 AgentTeams 轮询）
      var sgEntries = (subagentsView && subagentsView.nodes) || []
      var sgEntry = sgEntries[0]
      var graphState = sgEntry !== undefined && sgEntry.data !== undefined
        ? sgEntry.data
        : { rootSessionId: undefined, openedAt: 0, nodes: {} }

      var main = state
        ? h(BranchExplorePanel, { state: state, invoke: invoke })
        : h('div', { style: Object.assign({}, muted, { padding: 40, textAlign: 'center' }) }, '暂无 Parallel Worlds 运行')

      return h('div', {
        className: 'worlds-view',
        style: {
          height: '100%',
          boxSizing: 'border-box',
          display: 'flex',
          flexDirection: 'column',
        },
      },
        h('style', null, worldsLayoutCss),

        runSelector,

        h('div', {
          className: 'worlds-layout',
          style: {
            flex: 1,
            minHeight: 0,
          },
        },
          h('div', {
            className: 'worlds-main',
            style: {
              overflowY: 'auto',
              padding: 12,
              boxSizing: 'border-box',
            },
          }, main),

          h('div', {
            className: 'worlds-exec',
            style: {
              overflowY: 'auto',
              padding: 12,
              boxSizing: 'border-box',
              borderLeft: '1px solid #e1e4e8',
            },
          },
            h('div', { style: { display: 'flex', gap: 2, borderBottom: '1px solid #e1e4e8', marginBottom: 10 } },
              h('button', {
                type: 'button',
                onClick: function () { setRightTab('exec') },
                style: tabStyle(rightTab === 'exec'),
              }, '执行图'),
              h('button', {
                type: 'button',
                onClick: function () { setRightTab('version') },
                style: tabStyle(rightTab === 'version'),
              }, '版本分支'),
            ),
            rightTab === 'exec'
              ? h(SubagentGraphPanel, { state: graphState })
              : h(VersionBranchGraph, { entries: entries }),
          ),
        ),
      )
    }

    // ── 客户端插件注册 ──
    // 客户端 cordis 模块规范：模块必须导出 apply(ctx)，
    // 否则浏览器端加载器会报 invalid plugin, expect function or object with an "apply" method。

    // dsh 0.1.5-rc.2：conversationEvents / conversationViews 两个独立服务已并入
    // uiConversation 聚合服务（events / views 注册表，
    // 见 dsh 的 packages/client/ui-conversation/src/client/conversation/assembly.ts）。
    var inject = ['uiConversation', 'slots', 'remote', 'remote.commands', 'sessions']

    function apply(ctx) {
      var sessions = ctx.sessions
      // 事件聚合：把 exploration-* / variant-* 事件折叠成一张聊天卡片（摘要）
      ctx.uiConversation.events.register(branchExploreDefinition)
      // 事件聚合：同一批事件再投影一份到 'worlds' 视图（全宽 Dashboard）
      ctx.uiConversation.events.register(worldsEventDefinition)
      // 事件聚合：全局子代理图（subagent-graph-opened + subagent-*），投递到 'subagents' 视图
      ctx.uiConversation.events.register(subagentGraphDefinition)
      // 视图数据源
      ctx.uiConversation.views.register(subagentViewDefinition)
      ctx.uiConversation.views.register(worldsViewDefinition)
      // 卡片渲染：注册到 conversation.chat.node 槽位，key 必须与事件聚合的 kind 一致
      ctx.slots.inject('conversation.chat.node', () => ctx.slots.register({
        name: 'conversation.chat.node',
        key: 'branch-explore',
        inject: function (sessionId) {
          return {
            executeCommand: function (line) {
              // commands/execute 签名是 (agentId, line, images)——三参必需，
              // images 不用时传空数组。
              return ctx.remote.commands.execute(sessionId, line, [])
            },
            openSession: function (targetId) {
              sessions.open(targetId)
            },
          }
        },
      }, BranchExploreCard))
      // 视图 tab：Chat | 轨迹 | Worlds
      ctx.slots.inject('conversation.view', () => ctx.slots.register({
        name: 'conversation.view',
        id: 'worlds',
        order: 15,
        label: () => 'Worlds',
        inject: function (sessionId) {
          return {
            executeCommand: function (line) {
              return ctx.remote.commands.execute(sessionId, line, [])
            },
            openSession: function (targetId) {
              sessions.open(targetId)
            },
          }
        },
      }, WorldsView))
    }

    // ── 导出 ──

    exports.apply = apply
    exports.inject = inject
    exports.BranchExplorePanel = BranchExplorePanel
    exports.branchExploreDefinition = branchExploreDefinition
    exports.worldsEventDefinition = worldsEventDefinition
    exports.WorldsView = WorldsView
    exports.worldsViewDefinition = worldsViewDefinition
    exports.BranchExploreSummary = BranchExploreSummary
    exports.SubagentGraphPanel = SubagentGraphPanel
    exports.subagentGraphDefinition = subagentGraphDefinition
    exports.SubagentView = SubagentView
    exports.subagentViewDefinition = subagentViewDefinition

    return module.exports
  },
})