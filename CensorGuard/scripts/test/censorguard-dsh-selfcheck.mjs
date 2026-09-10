#!/usr/bin/env node
// censorguard-dsh-selfcheck: dsh.sock 自助面测试辅助 (attach_self/status_self/tree_self/raw)
// 走插件 src/runtime 的真实 AttachSelfClient (v2 NDJSON 信封),
// 与 ohmyguard packages/runtime/dist/selfcheck.js 对齐。
//
// 用法:
//   node censorguard-dsh-selfcheck.mjs attach <sock> <policyGroup> [instanceHint]
//   node censorguard-dsh-selfcheck.mjs raw <sock> <method> [paramsJson]
// 退出码: 0 = attach/调用成功; 1 = 失败 (错误打到 stderr)

import { randomUUID } from 'node:crypto';
import net from 'node:net';
import { AttachSelfClient, AttachError } from '../../plugins/dsh-censorguard/dist/runtime/index.js';

const [, , command, sock, ...rest] = process.argv;

function rawCall(sockPath, method, params) {
  return new Promise((resolve, reject) => {
    const request = { v: 2, request_id: `selfcheck-${process.pid}-${randomUUID()}`, method, params };
    const conn = net.createConnection({ path: sockPath });
    const timer = setTimeout(() => {
      conn.destroy();
      reject(new Error('连接超时 (5000ms)'));
    }, 5000);
    conn.on('error', (err) => {
      clearTimeout(timer);
      reject(err);
    });
    conn.on('connect', () => conn.write(JSON.stringify(request) + '\n'));
    let buf = '';
    conn.on('data', (data) => {
      buf += data.toString('utf8');
      const nl = buf.indexOf('\n');
      if (nl < 0) return;
      clearTimeout(timer);
      conn.end();
      try {
        resolve(JSON.parse(buf.slice(0, nl)));
      } catch (e) {
        reject(e);
      }
    });
  });
}

async function main() {
  if (command === 'attach') {
    const [policyGroup, instanceHint] = rest;
    if (!sock || !policyGroup) {
      console.error('用法: attach <sock> <policyGroup> [instanceHint]');
      process.exit(1);
    }
    const client = new AttachSelfClient({
      dshSockPath: sock,
      policyGroup,
      instanceHint,
      seed: true,
    });
    try {
      const r = await client.attachSelf();
      console.log(`✅ attach 成功 → 域 "${r.domain}" id=${r.domainId} group=${r.group} version=${r.version}`);
      console.log(`   daemonBootId=${r.daemonBootId} hooksHealthy=${r.hooksHealthy}`);
    } catch (e) {
      if (e instanceof AttachError) {
        console.error(`❌ attach 失败 kind=${e.kind}: ${e.message}`);
      } else {
        console.error(`❌ attach 失败: ${e.message}`);
      }
      process.exit(1);
    } finally {
      client.dispose();
    }
    return;
  }

  if (command === 'raw') {
    const [method, paramsJson] = rest;
    if (!sock || !method) {
      console.error('用法: raw <sock> <method> [paramsJson]');
      process.exit(1);
    }
    try {
      const resp = await rawCall(sock, method, paramsJson ? JSON.parse(paramsJson) : {});
      console.log(JSON.stringify(resp, null, 2));
      if (!resp.ok) process.exit(3); // 业务拒绝: 输出在 stdout, 退出码区分
    } catch (e) {
      console.error(`❌ 调用失败: ${e.message}`);
      process.exit(1);
    }
    return;
  }

  console.error('未知命令, 用法: attach|raw (见文件头注释)');
  process.exit(1);
}

main();
