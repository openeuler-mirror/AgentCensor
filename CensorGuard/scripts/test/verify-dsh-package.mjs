#!/usr/bin/env node
// Check the actual tarball payload; external gRPC dependencies come from the local build install.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, symlinkSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runInNewContext } from 'node:vm';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const archive = resolve(process.argv[2] ?? join(repo, 'dist/censorguard-dsh-0.5.0.tgz'));
const directory = mkdtempSync(join(tmpdir(), 'cg-dsh-package-'));
execFileSync('tar', ['-xzf', archive, '-C', directory]);
const pkg = join(directory, 'package');
symlinkSync(join(repo, 'plugins/dsh-censorguard/node_modules'), join(pkg, 'node_modules'));
const manifest = JSON.parse(readFileSync(join(pkg, 'package.json'), 'utf8'));
assert.equal(manifest.name, '@censorguard/dsh');
assert.equal(manifest.version, '0.5.0');
for (const target of ['dist/bootstrap/index.js', 'dist/host/index.js', 'dist/ui/index.js']) {
  const module = await import(pathToFileURL(join(pkg, target)));
  assert.equal(typeof module.apply, 'function');
}
assert(existsSync(join(pkg, 'proto/censorguard/v1/censorguard.proto')));
await import(pathToFileURL(join(pkg, 'bin/dsh.mjs')));
let registration;
runInNewContext(readFileSync(join(pkg, 'lib/client.js'), 'utf8'), {
  TextEncoder,
  TextDecoder,
  window: { __ModuleLoader__: { load: value => { registration = value; } } },
});
assert.equal(registration.id, '@censorguard/dsh');
const browser = registration.factory(name => {
  assert(['react', 'react/jsx-runtime'].includes(name));
  return {};
});
const entries = [];
browser.apply({
  connection: { isLoopback: true },
  slots: { inject: (_key, fn) => fn(), register: options => { entries.push(options); return () => {}; } },
});
assert.equal(entries.map(entry => entry.id).join(','), 'security,security-audit');
console.log('PASS packed Node entries, proto, CLI helper and browser slot registrations');
console.log(`Artifacts: ${directory}`);
