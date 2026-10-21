// Bounded HTTP/1.1 JSON transports. TLS direct always uses the actual OpenSSL hook.
import http from 'node:http';
import https from 'node:https';
import { spawn } from 'node:child_process';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';

async function privateKey(path) {
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await file.stat();
    if (!info.isFile() || (info.mode & 0o077) || info.size > 8192 || ![0, process.getuid()].includes(info.uid)) throw new Error('credential_invalid');
    const key = (await file.readFile('utf8')).trim();
    if (!/^[\x21-\x7e]+$/.test(key)) throw new Error('credential_invalid');
    return key;
  } finally { await file.close(); }
}

export function requestJSON(url, body, { token, timeoutMs = 90000, signal } = {}) {
  const target = new URL(url);
  if (target.username || target.password || !['http:', 'https:'].includes(target.protocol)) throw new Error('endpoint_invalid');
  const data = Buffer.from(JSON.stringify(body));
  if (data.length > 1024 * 1024 || !Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 180000) throw new Error('request_limit');
  return new Promise((resolve, reject) => {
    let timer;
    const succeed = value => { clearTimeout(timer); resolve(value); };
    const fail = failure => { clearTimeout(timer); reject(failure); };
    const request = (target.protocol === 'http:' ? http : https).request(target, {
      method: 'POST', signal, agent: false,
      headers: {
        'content-type': 'application/json', 'content-length': data.length,
        'accept-encoding': 'identity', 'user-agent': 'deepseek-harness (CensorPrivacy/0.3.0)',
        ...(token ? { authorization: `Bearer ${token}` } : {}),
      },
    }, response => {
      const chunks = []; let size = 0;
      response.on('data', chunk => {
        size += chunk.length;
        if (size > 1024 * 1024) response.destroy(new Error('response_limit'));
        else chunks.push(chunk);
      });
      response.on('error', () => fail(new Error('transport_failed')));
      response.on('end', () => {
        if (response.statusCode !== 200) return fail(new Error(`upstream_http_${response.statusCode}`));
        if (response.headers['content-encoding'] && response.headers['content-encoding'] !== 'identity') return fail(new Error('response_encoding_invalid'));
        try { succeed(JSON.parse(Buffer.concat(chunks))); } catch { fail(new Error('response_invalid')); }
      });
    });
    timer = setTimeout(() => request.destroy(new Error('timeout')), timeoutMs);
    request.on('error', () => fail(new Error(signal?.aborted ? 'aborted' : 'transport_failed')));
    request.end(data);
  });
}

export async function tlsJSON(config, body, signal) {
  const target = new URL(config.baseURL);
  if (target.protocol !== 'https:' || !/^\d+\.\d+\.\d+\.\d+$/.test(target.hostname) || target.pathname !== '/' || target.search || target.hash || target.username || target.password) throw new Error('tls_ipv4_origin_required');
  const timeoutMs = config.timeoutMs ?? 90000;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 180000 || Buffer.byteLength(JSON.stringify(body)) > 1024 * 1024) throw new Error('request_limit');
  const args = ['--broker', config.broker, '--token-file', config.tokenFile,
    '--run-state-file', config.runStateFile, '--native-library', config.nativeLibrary,
    '--workspace', config.workspace, '--timeout', String(Math.ceil(timeoutMs / 1000) + 5)];
  if (config.upstreamKeyFile) args.push('--upstream-key-file', config.upstreamKeyFile);
  args.push('--', '/usr/bin/curl', '--config', '-');
  // curl config travels through stdin, so neither body nor credential enters argv.
  const lines = ['silent', 'show-error', 'fail', 'http1.1', 'noproxy = "*"',
    `max-time = ${Math.ceil(timeoutMs / 1000)}`, `url = ${JSON.stringify(`${target.origin}/v1/chat/completions`)}`,
    'header = "Content-Type: application/json"', 'header = "Accept-Encoding: identity"',
    'header = "Connection: close"', 'user-agent = "deepseek-harness (CensorPrivacy/0.3.0)"',
    `data = ${JSON.stringify(JSON.stringify(body))}`];
  if (config.caFile) lines.push(`cacert = ${JSON.stringify(config.caFile)}`);
  if (config.upstreamKeyFile) lines.push(`header = ${JSON.stringify(`Authorization: Bearer ${await privateKey(config.upstreamKeyFile)}`)}`);
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const child = spawn(config.tlsRun, args, { stdio: ['pipe', 'pipe', 'pipe'] });
    const chunks = []; let size = 0; let exceeded = false; let launchFailed = false;
    const abort = () => child.kill('SIGTERM');
    signal?.addEventListener('abort', abort, { once: true });
    const timer = setTimeout(() => child.kill('SIGTERM'), timeoutMs + 10000);
    child.stdout.on('data', chunk => {
      size += chunk.length;
      if (size > 1024 * 1024) { exceeded = true; child.kill('SIGTERM'); }
      else chunks.push(chunk);
    });
    // tls-run diagnostics are fixed, but do not expose untrusted curl/provider output.
    child.stderr.resume();
    child.stdin.on('error', () => {}); // child exit closes stdin; close owns result.
    child.on('error', () => { launchFailed = true; });
    child.on('close', code => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      if (launchFailed || code !== 0 || exceeded || signal?.aborted) return reject(new Error(signal?.aborted ? 'aborted' : 'tls_request_blocked'));
      try { resolve(JSON.parse(Buffer.concat(chunks))); } catch { reject(new Error('response_invalid')); }
    });
    child.stdin.end(`${lines.join('\n')}\n`);
    if (signal?.aborted) abort();
  });
}
