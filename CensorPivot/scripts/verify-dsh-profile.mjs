#!/usr/bin/env node
// Compose the shipped tarballs with the actual DSH CLI, in an isolated DSH_HOME.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdtempSync, mkdirSync, readdirSync, readFileSync, symlinkSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const packages = resolve(process.argv[2]);
const dsh = resolve(process.argv[3] ?? process.env.DSH_ROOT);
const work = mkdtempSync(join(tmpdir(), 'pivot-profile-'));
const requireCli = createRequire(join(dsh, 'apps/cli/package.json'));
const yaml = requireCli('js-yaml');
const schema = yaml.DEFAULT_SCHEMA.extend(new yaml.Type('tag:yaml.org,2002:js', { kind: 'scalar', construct: value => value }));
try {
  const sources = new Map();
  for (const archive of readdirSync(packages).filter(file => file.endsWith('.tgz'))) {
    const target = join(work, archive);
    mkdirSync(target);
    execFileSync('tar', ['-xzf', join(packages, archive), '-C', target]);
    const source = join(target, 'package');
    const manifest = JSON.parse(readFileSync(join(source, 'package.json'), 'utf8'));
    sources.set(manifest.name, source);
    if (manifest.name === '@censorguard/dsh') {
      assert.equal(manifest.version, '0.5.0');
      assert(!manifest.dsh.bundle);
      assert(manifest.dsh.client);
      assert(readFileSync(join(source, 'dist/host/auditStream.js')).length > 100);
      assert(readFileSync(join(source, 'lib/client.js')).length > 1000);
    }
  }
  for (const profile of ['web', 'headless']) {
    const dir = join(work, 'home/profiles', profile);
    const modules = join(dir, 'node_modules');
    mkdirSync(modules, { recursive: true });
    for (const entry of readdirSync(join(dsh, 'apps/cli/node_modules'))) {
      if (entry === '@agentcensor' || entry === '@censorguard' || entry === '@censorfs') continue;
      symlinkSync(join(dsh, 'apps/cli/node_modules', entry), join(modules, entry));
    }
    const dependencies = {};
    for (const [name, source] of sources) {
      const target = join(modules, name);
      if (name.startsWith('@')) mkdirSync(join(modules, name.split('/')[0]), { recursive: true });
      symlinkSync(source, target);
      dependencies[name] = `link:${source}`;
    }
    const bundles = ['@deepseek-ai/dsh-base', profile === 'web' ? '@deepseek-ai/dsh-web-app' : '@deepseek-ai/dsh-headless',
      profile === 'web' ? '@agentcensor/censorpivot' : '@agentcensor/censorpivot-headless'];
    writeFileSync(join(dir, 'package.json'), JSON.stringify({ private: true, dependencies, dsh: { profile: { bundles } } }));
    const dump = execFileSync(process.execPath, ['--import', requireCli.resolve('tsx/esm'), 'apps/cli/src/bin.ts', '--profile', profile, '--dump-config'],
      { cwd: dsh, encoding: 'utf8', env: { ...process.env, DSH_HOME: join(work, 'home') }, timeout: 30000 });
    writeFileSync(join(work, `${profile}.yaml`), dump);
    assert(!/patch: entry .+ not found/.test(dump), dump);
    const entries = yaml.load(dump, { schema });
    const byId = new Map(entries.map(entry => [entry.id, entry]));
    if (profile === 'web') {
      for (const id of ['censorguard-bootstrap', 'censorguard-host', 'censorguard-ui', 'censorfs-parallel-worlds', 'censorscope-host', 'agentcensor-session-proxy', 'agentcensor-skill-filesystem', 'skill']) assert(byId.has(id), `missing ${id}`);
      assert(!byId.has('censorscope-ui'), 'standalone Scope UI should not be active');
      assert.notEqual(byId.get('ui-trajectory')?.disabled, true, 'native trajectory should remain available');
      assert.equal(byId.get('censorguard-bootstrap').config.blockOnFailure, true);
      for (const id of ['webserver', 'web-runtime', 'typert-gateway', 'cordis-host-runner', 'code-runtime', 'subprocess', 'tools', 'agent-loop', 'censorfs-parallel-worlds', 'agentcensor-session-proxy']) {
        assert(byId.get(id)?.inject.includes('censorguardReady'), `unguarded ${id}`);
      }
      for (const service of ['agents', 'sessions', 'sessionPersistence', 'sessionProjections']) assert(byId.get('agentcensor-session-proxy').inject.includes(service));
      assert.equal(byId.get('agent-loop').disabled, true);
      assert.notEqual(byId.get('subprocess').disabled, true);
    } else {
      assert.notEqual(byId.get('agent-loop')?.disabled, true);
      assert(!byId.has('agentcensor-session-proxy'));
      assert(!byId.has('censorguard-bootstrap'));
      assert(byId.has('censorscope-host'));
    }
    console.log(`PASS shipped ${profile} composition with the current DSH CLI`);
  }
} finally { rmSync(work, { recursive: true, force: true }); }
