#!/usr/bin/env python3
"""Run the documented installer with a private profile and cached dependencies."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[2]
DSH = Path(sys.argv[1] if len(sys.argv) > 1 else '/root/deepseek-harness').resolve()
directory = Path(tempfile.mkdtemp(prefix='cg-dsh-install-'))
env = {**os.environ, 'DSH_HOME': str(directory / 'home'), 'npm_config_offline': 'true'}
print(f'Artifacts: {directory}', flush=True)


def install(label):
    result = subprocess.run(['make', 'plugin-install', f'DSH_REPO={DSH}'], cwd=REPO,
                            env=env, text=True, capture_output=True, timeout=120)
    (directory / f'{label}.log').write_text(result.stdout + result.stderr)
    assert result.returncode == 0, result.stdout + result.stderr


install('first')
profile = directory / 'home/profiles/web'
manifest = json.loads((profile / 'package.json').read_text())
assert '@censorguard/dsh' in manifest['dependencies']
assert '@censorguard/dsh' in manifest['dsh']['profile']['bundles']
installed = profile / 'node_modules/@censorguard/dsh'
assert json.loads((installed / 'package.json').read_text())['version'] == '0.5.0'
assert (installed / 'lib/client.js').is_file()
# Repeat the same-version install: the path to the tarball is intentionally stable.
install('repeat')
legacy = ['dsh-censorguard', '@censorguard/dsh-bundle', '@censorguard/dsh-bootstrap',
          '@censorguard/dsh-host', '@censorguard/dsh-client']
manifest = json.loads((profile / 'package.json').read_text())
for name in legacy:
    manifest['dependencies'][name] = f'file:{directory}/deleted-{name.rsplit("/", 1)[-1]}.tgz'
    manifest['dsh']['profile']['bundles'].append(name)
(profile / 'package.json').write_text(json.dumps(manifest))
install('legacy-migration')
manifest = json.loads((profile / 'package.json').read_text())
assert all(name not in manifest['dependencies'] for name in legacy)
assert all(name not in manifest['dsh']['profile']['bundles'] for name in legacy)
check = subprocess.run(['node', 'plugins/dsh-censorguard/bin/dump-config.mjs', '--dsh', str(DSH)],
                       cwd=REPO, env=env, text=True, capture_output=True, timeout=60)
(directory / 'dump.log').write_text(check.stdout + check.stderr)
assert check.returncode == 0, check.stdout + check.stderr
runtime = subprocess.run(['node', '--input-type=module', '-e', '''
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
const pkg = process.argv[1];
const require = createRequire(join(pkg, 'package.json'));
for (const entry of ['dist/bootstrap/index.js', 'dist/host/index.js', 'dist/ui/index.js']) {
  assert.equal(typeof (await import(pathToFileURL(join(pkg, entry)))).apply, 'function');
}
const definition = require('@grpc/proto-loader').loadSync(join(pkg, 'proto/censorguard/v1/censorguard.proto'));
assert.equal(typeof require('@grpc/grpc-js').loadPackageDefinition(definition).censorguard.v1.Censorguard, 'function');
''', str(installed)], cwd=REPO, env=env, text=True, capture_output=True, timeout=20)
assert runtime.returncode == 0, runtime.stdout + runtime.stderr
print('PASS first installation, same-version reinstall, dangling legacy migration and all patch gates')
print('PASS installed runtime entries and gRPC schema with lifecycle scripts skipped')
