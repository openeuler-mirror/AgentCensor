import { requestJSON, tlsJSON } from './transport.mjs';
import { open } from 'node:fs/promises';
import { constants } from 'node:fs';

export const name = 'censorprivacy';
export const inject = ['llm', 'tools'];
const ref = /^(?:<CP_[A-Z][A-Z0-9_]*_[0-9a-f]{32}>|~P[A-Z]{10,}|~[A-Z]{5,10}|[A-Z]{3,5})$/;
const error = code => Object.assign(new Error(`CensorPrivacy: ${code}`), { code });
const operations = ['sum', 'mean', 'min', 'max', 'aggregate', 'date_diff'];
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const keys = (value, allowed) => object(value) && Object.keys(value).every(key => allowed.includes(key));

async function receipt(config, event) {
  if (!config.receiptFile) return;
  const file = await open(config.receiptFile, constants.O_WRONLY | constants.O_CREAT | constants.O_APPEND | constants.O_NOFOLLOW | constants.O_NONBLOCK, 0o600);
  try {
    const info = await file.stat();
    if (!info.isFile() || info.uid !== process.getuid() || (info.mode & 0o077) || info.size > 1024 * 1024) throw error('RECEIPT_FILE_INVALID');
    await file.writeFile(JSON.stringify({ ...event, timestamp: new Date().toISOString() }) + '\n');
  } finally { await file.close(); }
}

export function privateResult(result, operation) {
  if (!keys(result, ['operation', 'results', 'result', 'entity_type', 'unit']) || result.operation !== operation ||
      !object(result.results) || !Object.keys(result.results).length ||
      Object.entries(result.results).some(([key, value]) => !['sum', 'mean', 'min', 'max', 'date_diff'].includes(key) || typeof value !== 'string' || !ref.test(value)) ||
      !['MONEY', 'NUMBER', 'DURATION'].includes(result.entity_type) ||
      (result.unit !== undefined && !['CNY', 'USD', 'EUR', 'GBP', 'JPY', 'days'].includes(result.unit)) ||
      (result.result !== undefined && !Object.values(result.results).includes(result.result))) throw error('PRIVATE_RESULT_REQUIRED');
  return result;
}

function text(blocks) {
  if (!Array.isArray(blocks) || blocks.some(block => !keys(block, ['type', 'text']) || block.type !== 'text' || typeof block.text !== 'string')) throw error('UNSUPPORTED_CONTENT');
  return blocks.map(block => block.text).join('\n');
}
export function serialize(options, config) {
  if (options.reasoningEffort !== undefined) throw error('UNSUPPORTED_OPTION');
  if (!Array.isArray(options.messages) || (options.system !== undefined && typeof options.system !== 'string')) throw error('UNSUPPORTED_CONTENT');
  const messages = [];
  if (options.system !== undefined) messages.push({ role: 'system', content: options.system });
  for (const message of options.messages) {
    if (!['system', 'user', 'assistant'].includes(message.role)) throw error('UNSUPPORTED_CONTENT');
    const blocks = message.content;
    if (!Array.isArray(blocks)) throw error('UNSUPPORTED_CONTENT');
    if (message.role === 'assistant') {
      const calls = blocks.filter(block => block.type === 'tool-call');
      if (calls.some(call => !keys(call, ['type', 'id', 'name', 'arguments']) || ![call.id, call.name, call.arguments].every(value => typeof value === 'string'))) throw error('UNSUPPORTED_CONTENT');
      const content = text(blocks.filter(block => block.type !== 'tool-call'));
      messages.push({ role: 'assistant', content: content || null,
        ...(calls.length ? { tool_calls: calls.map(call => ({ id: call.id, type: 'function', function: { name: call.name, arguments: call.arguments } })) } : {}) });
    } else {
      let pending = [];
      const flush = () => { if (pending.length) { messages.push({ role: message.role, content: text(pending) }); pending = []; } };
      for (const block of blocks) {
        if (block.type === 'tool-result') {
          if (!keys(block, ['type', 'toolCallId', 'content', 'isError']) || typeof block.toolCallId !== 'string') throw error('UNSUPPORTED_CONTENT');
          flush(); messages.push({ role: 'tool', tool_call_id: block.toolCallId, content: text(block.content) });
        }
        else pending.push(block);
      }
      flush();
    }
  }
  return { model: options.model, messages, stream: false,
    ...(options.tools?.length ? { tools: options.tools.map(tool => ({ type: 'function', function: { name: tool.name, description: tool.description, parameters: tool.parameters } })) } : {}),
    max_tokens: options.maxTokens ?? config.maxTokens,
    ...(options.temperature === undefined ? {} : { temperature: options.temperature }),
    ...(options.stop === undefined ? {} : { stop: options.stop }) };
}

export class PrivacyAdapter {
  constructor(config) { this.config = config; }
  providerInfo(provider) { return { id: provider, name: 'CensorPrivacy' }; }
  providerRetryPolicy() { return undefined; }
  imageRequestPricing() { return undefined; }
  async listModels(provider) { return [{ provider, id: this.config.model, name: this.config.model, inputModalities: ['text'] }]; }
  async resolveModel(provider, model) {
    if (model !== this.config.model) throw error('MODEL_NOT_CONFIGURED');
    return { provider, id: model, name: model, inputModalities: ['text'], context: { contextWindow: this.config.contextWindow }, defaultMaxTokens: this.config.maxTokens };
  }
  async prepareCall(provider, model) { return { model: await this.resolveModel(provider, model), stream: options => this.stream(options) }; }
  async *stream(options) {
    const config = this.config;
    let result;
    try {
      const body = serialize(options, config);
      result = config.mode === 'tls' ? await tlsJSON(config, body, options.signal)
        : await requestJSON(`${config.baseURL.replace(/\/$/, '')}/v1/chat/completions`, body,
          { token: process.env.CENSORPRIVACY_TOKEN, timeoutMs: config.timeoutMs, signal: options.signal });
    } catch (failure) {
      if (['UNSUPPORTED_CONTENT', 'UNSUPPORTED_OPTION'].includes(failure?.code)) throw failure;
      throw error(/^upstream_http_[0-9]{3}$/.test(failure?.message) ? failure.message.toUpperCase() : 'PRIVACY_TRANSPORT_FAILED');
    }
    const choice = result?.choices?.[0]; const message = choice?.message;
    if (!Array.isArray(result?.choices) || result.choices.length !== 1 ||
        !keys(message, ['role', 'content', 'tool_calls', 'refusal', 'reasoning_content']) || message.role !== 'assistant' ||
        (message.reasoning_content != null && message.reasoning_content !== '') ||
        (message.refusal != null && message.refusal !== '') ||
        (message.content != null && typeof message.content !== 'string') ||
        (message.tool_calls !== undefined && !Array.isArray(message.tool_calls)) ||
        !['stop', 'tool_calls', 'length'].includes(choice.finish_reason)) throw error('UNSUPPORTED_RESPONSE');
    if (result.usage !== undefined && (!object(result.usage) || ['prompt_tokens', 'completion_tokens'].some(key => result.usage[key] !== undefined && (!Number.isSafeInteger(result.usage[key]) || result.usage[key] < 0)))) throw error('UNSUPPORTED_RESPONSE');
    for (const call of message.tool_calls ?? []) {
      if (!keys(call, ['id', 'type', 'function']) || call.type !== 'function' || typeof call.id !== 'string' ||
          !keys(call.function, ['name', 'arguments']) || typeof call.function.name !== 'string' || typeof call.function.arguments !== 'string') throw error('UNSUPPORTED_RESPONSE');
    }
    await receipt(config, { event: 'model_response', mode: config.mode, tool_calls: message.tool_calls?.length ?? 0 });
    let index = 0;
    if (typeof message.content === 'string' && message.content.length) {
      yield { type: 'block-start', index, blockType: 'text' };
      yield { type: 'text-delta', index, text: message.content };
      yield { type: 'block-end', index: index++, block: { type: 'text', text: message.content } };
    }
    for (const call of message.tool_calls ?? []) {
      const block = { type: 'tool-call', id: call.id, name: call.function.name, arguments: call.function.arguments };
      yield { type: 'block-start', index, blockType: 'tool-call' };
      yield { type: 'tool-call-delta', index, id: block.id, name: block.name, argumentsDelta: block.arguments };
      yield { type: 'block-end', index: index++, block };
    }
    if (result.usage) yield { type: 'usage', usage: { inputTokens: result.usage.prompt_tokens ?? 0, outputTokens: result.usage.completion_tokens ?? 0 } };
    yield { type: 'finish', reason: { kind: choice.finish_reason === 'tool_calls' ? 'tool-calls' : choice.finish_reason === 'length' ? 'max-tokens' : 'stop' } };
  }
}

export function apply(ctx, supplied) {
  const config = { timeoutMs: 90000, maxTokens: 512, contextWindow: 32768, ...supplied };
  if (!['gateway', 'tls'].includes(config.mode) || !config.baseURL || !config.model || !config.broker || !process.env.CENSORPRIVACY_TOKEN) throw error('CONFIGURATION_REQUIRED');
  if (config.mode === 'gateway' && !/^http:\/\/127\.0\.0\.1:\d+$/.test(config.baseURL)) throw error('LOCAL_GATEWAY_REQUIRED');
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(config.broker)) throw error('LOCAL_BROKER_REQUIRED');
  if (![config.timeoutMs, config.maxTokens, config.contextWindow].every(value => Number.isSafeInteger(value) && value > 0) || config.timeoutMs > 180000 || config.maxTokens > 8192) throw error('CONFIGURATION_INVALID');
  if (config.mode === 'tls' && ['tlsRun', 'nativeLibrary', 'tokenFile', 'runStateFile', 'workspace'].some(key => !config[key])) throw error('TLS_CONFIGURATION_REQUIRED');
  ctx.llm.registerAdapter(['censorprivacy'], new PrivacyAdapter(config));
  ctx.tools.register({
    name: 'private_compute',
    description: 'Compute using current private references only. Preserve references exactly; never send raw operands. Results remain private references.',
    parameters: { type: 'object', properties: {
      operation: { type: 'string', enum: ['sum', 'mean', 'min', 'max', 'aggregate', 'date_diff'] },
      refs: { type: 'array', items: { type: 'string' }, minItems: 1, maxItems: 256 },
      options: { type: 'object', additionalProperties: true },
    }, required: ['operation', 'refs'], additionalProperties: false },
    output: { schema: { type: 'object', additionalProperties: true }, render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }] },
    async execute(args, exec) {
      if (!keys(args, ['operation', 'refs', 'options']) || !operations.includes(args.operation) || !Array.isArray(args.refs) || !args.refs.length || args.refs.length > 256 || args.refs.some(value => typeof value !== 'string' || !ref.test(value))) throw error('REFERENCES_REQUIRED');
      const result = privateResult(await requestJSON(`${config.broker}/v1/private_compute`, args,
        { token: process.env.CENSORPRIVACY_TOKEN, timeoutMs: config.timeoutMs, signal: exec.signal }), args.operation);
      await receipt(config, { event: 'private_compute', operation: args.operation, ref_count: args.refs.length, result_count: Object.keys(result.results).length, arguments_are_references: true, results_are_references: true });
      return result;
    },
  });
  if (config.privateToolsOnly) {
    ctx.on('agent/created', ({ agent }) => agent.ctx.tools.restrict({ allow: ['private_compute'] }));
    ctx.tools.guard(exec => exec.name === 'private_compute' ? undefined : 'CensorPrivacy: PRIVATE_TOOLS_ONLY');
  }
}
