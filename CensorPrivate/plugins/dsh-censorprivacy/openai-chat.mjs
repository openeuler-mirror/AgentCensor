// Chat Completions wire validation. No Harness events or privacy transformations.
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const keys = (value, allowed) => object(value) && Object.keys(value).every(key => allowed.includes(key));
const validIndex = value => Number.isSafeInteger(value) && value >= 0;
const invalid = () => { throw new Error('response_invalid'); };
const reasons = ['stop', 'tool_calls', 'length'];

function usage(value) {
  if (!object(value) || ['prompt_tokens', 'completion_tokens'].some(key => value[key] !== undefined && !validIndex(value[key]))) invalid();
  return { type: 'usage', inputTokens: value.prompt_tokens ?? 0, outputTokens: value.completion_tokens ?? 0 };
}
function tool(call) {
  if (!keys(call, ['id', 'type', 'function', 'index']) || call.type !== 'function' || typeof call.id !== 'string' || !call.id ||
      (call.index !== undefined && !validIndex(call.index)) ||
      !keys(call.function, ['name', 'arguments']) || typeof call.function.name !== 'string' || !call.function.name || typeof call.function.arguments !== 'string') invalid();
  let args;
  try { args = JSON.parse(call.function.arguments); } catch { invalid(); }
  if (!object(args)) invalid();
  return { type: 'tool-call', id: call.id, name: call.function.name, arguments: call.function.arguments };
}

export function* chatJSON(result) {
  const choice = result?.choices?.[0], message = choice?.message;
  if (!Array.isArray(result?.choices) || result.choices.length !== 1 ||
      !keys(message, ['role', 'content', 'tool_calls', 'refusal', 'reasoning_content']) || message.role !== 'assistant' ||
      (message.reasoning_content != null && message.reasoning_content !== '') || (message.refusal != null && message.refusal !== '') ||
      (message.content != null && typeof message.content !== 'string') ||
      (message.tool_calls !== undefined && !Array.isArray(message.tool_calls)) || !reasons.includes(choice.finish_reason)) invalid();
  // Validate the complete response before releasing any of its text.
  const calls = (message.tool_calls ?? []).map(tool);
  const tokens = result.usage === undefined ? undefined : usage(result.usage);
  if (message.content) yield { type: 'text', text: message.content };
  yield* calls;
  if (tokens) yield tokens;
  yield { type: 'finish', reason: choice.finish_reason };
}

async function* sseData(source) {
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let pending = '', data = [], event = '';
  // Handle CR/LF/CRLF, split UTF-8, comments and multiline data fields.
  for await (const bytes of source) {
    pending += decoder.decode(bytes, { stream: true });
    if (pending.length > 1024 * 1024) invalid();
    for (;;) {
      const end = pending.search(/[\r\n]/);
      if (end < 0 || (pending[end] === '\r' && end === pending.length - 1)) break;
      const line = pending.slice(0, end);
      pending = pending.slice(end + (pending[end] === '\r' && pending[end + 1] === '\n' ? 2 : 1));
      if (!line) {
        if (data.length) {
          if (event && event !== 'message') invalid();
          yield data.join('\n');
        }
        data = []; event = '';
      } else if (!line.startsWith(':')) {
        const colon = line.indexOf(':');
        const field = colon < 0 ? line : line.slice(0, colon);
        let value = colon < 0 ? '' : line.slice(colon + 1);
        if (value.startsWith(' ')) value = value.slice(1);
        if (field === 'data') data.push(value);
        else if (field === 'event') event = value;
      }
    }
  }
  pending += decoder.decode();
  if (pending.trim() || data.length) invalid();
}

export async function* chatSSE(source) {
  const calls = new Map();
  let reason, tokens;
  for await (const data of sseData(source)) {
    if (data === '[DONE]') {
      if (!reason || (calls.size && reason !== 'tool_calls') || (!calls.size && reason === 'tool_calls')) invalid();
      const completed = [...calls].sort(([a], [b]) => a - b).map(([, call]) => tool(call));
      if (new Set(completed.map(call => call.id)).size !== completed.length) invalid();
      yield* completed;
      if (tokens) yield tokens;
      yield { type: 'finish', reason };
      return;
    }
    let frame;
    try { frame = JSON.parse(data); } catch { invalid(); }
    if (!keys(frame, ['id', 'object', 'created', 'model', 'choices', 'usage', 'system_fingerprint', 'service_tier']) ||
        !Array.isArray(frame.choices) || frame.choices.length > 1) invalid();
    if (frame.usage != null) tokens = usage(frame.usage);
    if (!frame.choices.length) {
      if (frame.usage == null) invalid();
      continue;
    }
    const choice = frame.choices[0], delta = choice?.delta ?? {};
    if (!keys(choice, ['index', 'delta', 'finish_reason', 'logprobs']) || choice.index !== 0 || choice.logprobs != null ||
        !keys(delta, ['role', 'content', 'tool_calls', 'reasoning_content', 'refusal']) ||
        (delta.role !== undefined && delta.role !== 'assistant') ||
        (delta.reasoning_content != null && delta.reasoning_content !== '') || (delta.refusal != null && delta.refusal !== '') ||
        (delta.content != null && typeof delta.content !== 'string') ||
        (delta.tool_calls != null && !Array.isArray(delta.tool_calls)) || reason) invalid();
    if (choice.finish_reason != null && !reasons.includes(choice.finish_reason)) invalid();
    // Validate all members of this frame before yielding its text.
    for (const part of delta.tool_calls ?? []) {
      if (!keys(part, ['index', 'id', 'type', 'function']) || !validIndex(part.index) || part.index >= 64 ||
          (part.type !== undefined && part.type !== 'function') ||
          (part.id !== undefined && (typeof part.id !== 'string' || !part.id || part.id.length > 128)) ||
          (part.function !== undefined && !keys(part.function, ['name', 'arguments']))) invalid();
      const call = calls.get(part.index) ?? { type: 'function', function: { name: '', arguments: '' } };
      if (part.id !== undefined) {
        if (call.id !== undefined && call.id !== part.id) invalid();
        call.id = part.id;
      }
      for (const field of ['name', 'arguments']) {
        const fragment = part.function?.[field];
        if (fragment !== undefined) {
          if (typeof fragment !== 'string') invalid();
          call.function[field] += fragment;
        }
      }
      if (call.function.name.length > 128 || call.function.arguments.length > 1024 * 1024) invalid();
      calls.set(part.index, call);
    }
    if (delta.content) yield { type: 'text', text: delta.content };
    if (choice.finish_reason != null) reason = choice.finish_reason;
  }
  // A closed socket is not an affirmative end-of-response marker.
  invalid();
}
