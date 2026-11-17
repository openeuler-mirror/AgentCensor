import { requestJSON, requestChat, tlsJSON } from './transport.mjs';
import { chatJSON } from './openai-chat.mjs';
import { cliOutput } from './cli-output.mjs';
import { open } from 'node:fs/promises';
import { constants } from 'node:fs';

export const name = 'censorprivacy';
export const inject = ['llm', 'tools'];
const ref = /^(?:<CP_[A-Z][A-Z0-9_]*_[0-9a-f]{32}>|~P[A-Z]{10,}|~[A-Z]{5,10}|[A-Z]{3,5})$/;
const error = code => Object.assign(new Error(`CensorPrivacy: ${code}`), { code });
const operations = ['sum', 'mean', 'min', 'max', 'aggregate', 'date_diff'];
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const keys = (value, allowed) => object(value) && Object.keys(value).every(key => allowed.includes(key));
function validateComputeArguments(args) {
  if (!keys(args, ['operation', 'refs', 'options']) || !operations.includes(args.operation) ||
      !Array.isArray(args.refs) || !args.refs.length || args.refs.length > 256 ||
      args.refs.some(value => typeof value !== 'string' || !ref.test(value))) throw error('REFERENCES_REQUIRED');
  const options = args.options === undefined ? {} : args.options;
  if (!object(options)) throw error('COMPUTE_OPTIONS_INVALID');
  if (args.operation === 'date_diff') {
    if (args.refs.length !== 2) throw error('DATE_DIFF_REQUIRES_TWO_DATE_REFERENCES');
    if (!keys(options, ['unit']) || (options.unit !== undefined && options.unit !== 'days')) throw error('COMPUTE_OPTIONS_INVALID');
  } else if (args.operation === 'aggregate') {
    if (!keys(options, ['operations']) || (options.operations !== undefined &&
        (!Array.isArray(options.operations) || !options.operations.length || options.operations.length > 256 ||
         options.operations.some(value => !['sum', 'mean', 'min', 'max'].includes(value))))) throw error('COMPUTE_OPTIONS_INVALID');
  } else if (Object.keys(options).length) throw error('COMPUTE_OPTIONS_INVALID');
}

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
  // DSH may add agent-scoped tools after the global restriction was applied.
  // Keep the actual model-facing catalog consistent with the execution guard.
  const tools = config.privateToolsOnly ? options.tools?.filter(tool => tool.name === 'private_compute') : options.tools;
  return { model: options.model, messages, stream: config.mode !== 'tls',
    ...(typeof options.model === 'string' && options.model.startsWith('deepseek-') ? { thinking: { type: 'disabled' } } : {}),
    ...(tools?.length ? { tools: tools.map(tool => ({ type: 'function', function: { name: tool.name, description: tool.description, parameters: tool.parameters } })) } : {}),
    max_tokens: options.maxTokens ?? config.maxTokens,
    ...(options.temperature === undefined ? {} : { temperature: options.temperature }),
    ...(options.stop === undefined ? {} : { stop: options.stop }) };
}

export class PrivacyAdapter {
  constructor(config) { this.config = config; this.display = cliOutput(config.cliStream); }
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
    let index = 0, content = '', textOpen = false, toolCalls = 0;
    try {
      const body = serialize(options, config);
      // The Harness loop stamps the real session on each call. Keep it out of
      // the model payload; title/compaction calls are auxiliary, not chat turns.
      const session = options.sessionId ?? config.conversationSession;
      const conversation = session && !options.purpose
        ? { agent: 'deepseek-harness', session: String(session) } : undefined;
      const events = config.mode === 'tls' ? chatJSON(await tlsJSON(config, body, options.signal))
        : requestChat(`${config.baseURL.replace(/\/$/, '')}/v1/chat/completions`, body,
          { token: process.env.CENSORPRIVACY_TOKEN, timeoutMs: config.timeoutMs, signal: options.signal, conversation });
      for await (const event of events) {
        await this.display(event);
        if (event.type === 'text') {
          if (!textOpen) { yield { type: 'block-start', index, blockType: 'text' }; textOpen = true; }
          content += event.text;
          yield { type: 'text-delta', index, text: event.text };
        } else if (event.type === 'tool-call') {
          if (textOpen) {
            yield { type: 'block-end', index: index++, block: { type: 'text', text: content } };
            textOpen = false;
          }
          const block = { type: 'tool-call', id: event.id, name: event.name, arguments: event.arguments };
          yield { type: 'block-start', index, blockType: 'tool-call' };
          yield { type: 'tool-call-delta', index, id: block.id, name: block.name, argumentsDelta: block.arguments };
          yield { type: 'block-end', index: index++, block };
          toolCalls++;
        } else if (event.type === 'usage') {
          yield { type: 'usage', usage: { inputTokens: event.inputTokens, outputTokens: event.outputTokens } };
        } else if (event.type === 'finish') {
          if (textOpen) yield { type: 'block-end', index, block: { type: 'text', text: content } };
          await receipt(config, { event: 'model_response', mode: config.mode, tool_calls: toolCalls });
          yield { type: 'finish', reason: { kind: event.reason === 'tool_calls' ? 'tool-calls' : event.reason === 'length' ? 'max-tokens' : 'stop' } };
        }
      }
    } catch (failure) {
      if (['UNSUPPORTED_CONTENT', 'UNSUPPORTED_OPTION'].includes(failure?.code)) throw failure;
      if (failure?.message === 'response_invalid') throw error('UNSUPPORTED_RESPONSE');
      throw error(/^upstream_http_[0-9]{3}(?:_[a-z_]+)?$/.test(failure?.message) ? failure.message.toUpperCase() : 'PRIVACY_TRANSPORT_FAILED');
    }
  }
}

export function apply(ctx, supplied) {
  const config = { timeoutMs: 90000, maxTokens: 512, contextWindow: 32768, ...supplied };
  if (!['gateway', 'tls'].includes(config.mode) || !config.baseURL || !config.model || !config.broker || !process.env.CENSORPRIVACY_TOKEN) throw error('CONFIGURATION_REQUIRED');
  if (config.mode === 'gateway' && !/^http:\/\/127\.0\.0\.1:\d+$/.test(config.baseURL)) throw error('LOCAL_GATEWAY_REQUIRED');
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(config.broker)) throw error('LOCAL_BROKER_REQUIRED');
  if (![config.timeoutMs, config.maxTokens, config.contextWindow].every(value => Number.isSafeInteger(value) && value > 0) || config.timeoutMs > 180000 || config.maxTokens > 8192) throw error('CONFIGURATION_INVALID');
  if (config.mode === 'tls' && ['tlsRun', 'nativeLibrary', 'tokenFile', 'runStateFile', 'workspace'].some(key => !config[key])) throw error('TLS_CONFIGURATION_REQUIRED');
  if (config.conversationSession !== undefined && (typeof config.conversationSession !== 'string' || !/^[A-Za-z0-9_.:-]{1,128}$/.test(config.conversationSession))) throw error('CONFIGURATION_INVALID');
  ctx.llm.registerAdapter(['censorprivacy'], new PrivacyAdapter(config));
  ctx.tools.register({
    name: 'private_compute',
    description: 'Compute only when the user requests arithmetic. Use current private references only; copy each complete reference, including leading < and trailing >. date_diff requires exactly two DATE references and returns the second date minus the first in days. There is no implicit today/current-date operand: if a date is missing, ask the user instead of guessing. sum/mean/min/max accept numeric references and no options; aggregate optionally accepts operations. Never invent references or send raw operands. Results remain private references.',
    parameters: { type: 'object', properties: {
      operation: { type: 'string', enum: ['sum', 'mean', 'min', 'max', 'aggregate', 'date_diff'] },
      refs: { type: 'array', items: { type: 'string', pattern: ref.source }, minItems: 1, maxItems: 256 },
      options: { type: 'object', description: 'Omit for sum/mean/min/max. date_diff: optional unit days only. aggregate: optional operations only. No other keys are supported.',
        properties: { unit: { type: 'string', enum: ['days'] },
          operations: { type: 'array', items: { type: 'string', enum: ['sum', 'mean', 'min', 'max'] }, minItems: 1, maxItems: 256 } },
        additionalProperties: false },
    }, required: ['operation', 'refs'], additionalProperties: false },
    output: { schema: { type: 'object', additionalProperties: true }, render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }] },
    async execute(args, exec) {
      validateComputeArguments(args);
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
