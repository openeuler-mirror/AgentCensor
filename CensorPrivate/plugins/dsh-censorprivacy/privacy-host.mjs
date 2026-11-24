// Read-only bridge over DSH's authenticated Connection, with no browser-side
// gateway URL, credentials, arbitrary paths, or access to other Agent sources.
import { createHash } from 'node:crypto';

const failure = code => ({ ok: false, error: { code: 'gateway/internal', message: code, details: {} } });
const validSession = value => typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);

export function recordSessionId(session) {
  if (!validSession(session)) throw new Error('invalid_session');
  return createHash('sha256').update('censorprivacy-conversation-v1\0deepseek-harness\0')
    .update(session).digest('hex').slice(0, 32);
}

async function readRecord(base, id, signal) {
  const response = await fetch(`${base}/api/conversations/${id}`, {
    signal: AbortSignal.any([signal, AbortSignal.timeout(10000)]),
    redirect: 'error', cache: 'no-store', headers: { accept: 'application/json' },
  });
  try {
    if (response.status === 404) return { state: 'empty', exchanges: [] };
    if (!response.ok || !response.headers.get('content-type')?.startsWith('application/json')) throw new Error('record_unavailable');
    // The gateway bounds individual exchanges at 32 MiB. Bound the bridge too,
    // including when Content-Length is absent; never buffer an unbounded stream.
    let size = 0; const chunks = [];
    for await (const chunk of response.body) {
      size += chunk.length;
      if (size > 32 * 1024 * 1024) throw new Error('record_limit');
      chunks.push(chunk);
    }
    const record = JSON.parse(Buffer.concat(chunks));
    if (record.session_id !== id || !Array.isArray(record.exchanges) || record.exchanges.some(exchange =>
      exchange.session_id !== id || exchange.agent !== 'deepseek-harness' || !Array.isArray(exchange.snapshots))) {
      throw new Error('record_invalid');
    }
    // Only the exact text snapshots and display metadata are needed. Responses,
    // tool schemas, and other conversation data stay on the host.
    return { state: record.exchanges.length ? 'ready' : 'empty', exchanges: record.exchanges.map(exchange => ({
      id: exchange.id, created_at: exchange.created_at, model: exchange.model, status: exchange.status,
      snapshots: exchange.snapshots.map(snapshot => ({ ...snapshot,
        role: exchange.messages?.[Number(snapshot.path.match(/^\/messages\/(\d+)\//)?.[1])]?.role ?? 'context',
      })),
    })) };
  } finally { await response.body?.cancel().catch(() => {}); }
}

export function privacyHandler(config) {
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(config.broker)) throw new Error('local_broker_required');
  return async (endpoint, payload, signal, peer) => {
    if (peer?.principal?.kind !== 'owner') return failure('privacy_forbidden');
    if (endpoint !== 'session' || !payload || Array.isArray(payload) ||
      Object.keys(payload).length !== 1 || !validSession(payload.sessionId)) return failure('privacy_bad_request');
    if (config.mode === 'tls') return { ok: true, value: { state: 'tls', exchanges: [] } };
    try {
      return { ok: true, value: await readRecord(config.broker, recordSessionId(payload.sessionId), signal) };
    } catch {
      // Neither the upstream error body nor a credential is a UI diagnostic.
      return failure('privacy_unavailable');
    }
  };
}

export function registerPrivacyBridge(ctx, config) {
  // Headless profiles have neither service. Optional injection keeps the model
  // adapter available there, and ties route disposal to the plugin lifetime.
  ctx.inject(['connection', 'webServer'], web => {
    const handler = privacyHandler(config);
    // An exact Connection Fetch route uses the shared authenticated carrier,
    // which admits only owner Peers before invoking this callback. It also
    // avoids the dedicated-channel helper's dependency on its service context.
    web.effect(() => web.connection.fetch.register({
      path: '/api/censorprivacy/session', methods: ['POST'], requestBody: 'buffered',
      async fetch(request) {
        const message = await request.json().catch(() => null);
        const headers = { 'cache-control': 'no-store' };
        if (!message || message.type !== 'client-request' || typeof message.rpcId !== 'string' ||
          message.method !== 'censorprivacy/session') return new Response('invalid request', { status: 400, headers });
        const result = await handler('session', message.payload, request.signal, { principal: { kind: 'owner' } });
        return Response.json({ type: 'server-response', rpcId: message.rpcId, result }, { headers });
      },
    }));
  });
}
