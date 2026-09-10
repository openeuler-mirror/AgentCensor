#!/usr/bin/env node
// censorguard-grpc-call: grpcurl 的最小替代 (环境无 grpcurl 时用)
// 用 plugins/dsh-censorguard 已有的 @grpc/grpc-js + @grpc/proto-loader 加载
// api/censorguard/v1/censorguard.proto 发起一元调用。
//
// 用法: node censorguard-grpc-call.mjs <addr> <Method> [json]
//   例: node censorguard-grpc-call.mjs 127.0.0.1:50051 Status '{}'
// 成功: 响应 JSON 打 stdout, 退出 0
// 失败: "GRPC_ERROR <codeName>: <message>" 打 stderr, 退出 3

import { createRequire } from 'node:module';

// 从插件包的依赖里解析 grpc 库 (不新增任何依赖)
const require = createRequire(
  new URL('../plugins/dsh-censorguard/package.json', import.meta.url),
);
const grpc = require('@grpc/grpc-js');
const protoLoader = require('@grpc/proto-loader');

const PROTO = new URL('../api/censorguard/v1/censorguard.proto', import.meta.url).pathname;
const INCLUDE = new URL('../api', import.meta.url).pathname;

const [, , addr, method, json] = process.argv;
if (!addr || !method) {
  console.error('用法: censorguard-grpc-call.mjs <addr> <Method> [json]');
  process.exit(1);
}

const definition = protoLoader.loadSync(PROTO, {
  includeDirs: [INCLUDE],
  longs: String,
  enums: String,
  defaults: true,
});
const pkg = grpc.loadPackageDefinition(definition);
const Client = pkg.censorguard.v1.Censorguard;
const client = new Client(addr, grpc.credentials.createInsecure());

client.waitForReady(Date.now() + 5000, (readyErr) => {
  if (readyErr) {
    console.error(`GRPC_ERROR Unavailable: ${readyErr.message}`);
    process.exit(3);
  }
  client[method](json ? JSON.parse(json) : {}, (err, resp) => {
    if (err) {
      // err.codeName 在部分 grpc-js 版本不可靠, 用 status 枚举反查标准名
      const name = grpc.status[err.code] ?? String(err.code);
      console.error(`GRPC_ERROR ${name}: ${err.message}`);
      process.exit(3);
    }
    console.log(JSON.stringify(resp, null, 2));
    process.exit(0);
  });
});
