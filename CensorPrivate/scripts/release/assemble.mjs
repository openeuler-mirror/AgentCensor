// Build-time assembly only. The delivered service never invokes Cargo or Python.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync, chmodSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';

const flags = {};
for (let i = 2; i < process.argv.length; i += 2) {
  if (!['--directory', '--model-dir', '--ort-library', '--node', '--patchelf'].includes(process.argv[i]) || !process.argv[i + 1]) throw new Error('Invalid assembly argument');
  flags[process.argv[i].slice(2)] = process.argv[i + 1];
}
const root = resolve(import.meta.dirname, '../..');
const bundle = resolve(flags.directory);
const share = join(bundle, 'share/censorprivacy');
const libraries = join(bundle, 'lib/censorprivacy');
const notices = join(share, 'licenses');
const run = (command, args) => execFileSync(command, args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 }).trim();
const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');
function copy(source, destination, executable = false) {
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(source, destination);
  chmodSync(destination, executable ? 0o755 : 0o644);
}
function files(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(item => {
    const path = join(directory, item.name);
    if (item.isSymbolicLink()) throw new Error('Unexpected symlink in release: ' + path);
    return item.isDirectory() ? files(path) : [path];
  }).sort();
}
function json(path, value) { writeFileSync(path, JSON.stringify(value, null, 2) + '\n'); }
mkdirSync(libraries, { recursive: true });
mkdirSync(notices, { recursive: true });

const sourceModel = resolve(flags['model-dir']);
const manifest = JSON.parse(readFileSync(join(sourceModel, 'manifest.json')));
if (manifest.model_id !== 'shibing624/bert4ner-base-chinese' || manifest.precision !== 'float32') throw new Error('Unexpected model or precision');
for (const name of ['model.onnx', 'tokenizer.json', 'config.json']) {
  if (manifest.sha256[name] !== hash(join(sourceModel, name))) throw new Error('Model hash mismatch: ' + name);
}
for (const name of ['model.onnx', 'tokenizer.json', 'config.json', 'manifest.json', 'export-validation.json']) {
  copy(join(sourceModel, name), join(share, 'models/bert4ner-base-chinese', name));
}
copy(join(root, 'scripts/model/validated-model.json'), join(share, 'models/bert4ner-base-chinese/validated-model.json'));
cpSync(join(root, 'licenses/bert4ner-base-chinese'), join(notices, 'bert4ner-base-chinese'), { recursive: true });

const ort = realpathSync(flags['ort-library']);
const ortNotices = resolve(dirname(ort), '..');
for (const name of ['LICENSE', 'ThirdPartyNotices.txt']) copy(join(ortNotices, name), join(notices, 'onnxruntime', name));
const ortDestination = join(libraries, 'libonnxruntime.so');
copy(ort, ortDestination, true);
const provider = join(dirname(ort), 'libonnxruntime_providers_shared.so');
if (existsSync(provider)) copy(provider, join(libraries, basename(provider)), true);
copy(join(root, 'scripts/release/launcher.sh'), join(bundle, 'cpctl'), true);
const node = realpathSync(flags.node);
copy(resolve(dirname(node), '../LICENSE'), join(notices, 'node/LICENSE'));
const certFile = ['/etc/ssl/certs/ca-certificates.crt', '/etc/pki/tls/certs/ca-bundle.crt'].find(existsSync);
if (!certFile) throw new Error('No system CA bundle available');
copy(certFile, join(share, 'ca-bundle.crt'));

const elfFiles = files(join(bundle, 'bin')).filter(path => basename(path) !== 'cpctl');
elfFiles.push(...files(libraries));
const tools = {};
for (const name of ['node', 'jq', 'curl', 'openssl', 'awk', 'flock']) {
  const source = name === 'node' ? node : run('bash', ['-c', 'command -v "$1"', 'bash', name]);
  const destination = join(bundle, 'runtime/bin', name);
  copy(source, destination, true);
  elfFiles.push(destination);
  tools[name] = { binary_sha256_before_relocation: hash(source) };
}
tools.node.version = run(node, ['--version']);
const [nodeMajor, nodeMinor] = tools.node.version.slice(1).split('.').map(Number);
if (!(nodeMajor === 22 && nodeMinor >= 19 || nodeMajor >= 24)) throw new Error('Node 22.19+ (22.x), or 24+ required');
tools.jq.version = run(join(bundle, 'runtime/bin/jq'), ['--version']);
tools.curl.version = run(join(bundle, 'runtime/bin/curl'), ['--version']).split('\n')[0];
tools.openssl.version = run(join(bundle, 'runtime/bin/openssl'), ['version']);
if (!tools.curl.version.includes('OpenSSL/3.')) throw new Error('The TLS demonstration requires dynamic OpenSSL 3 curl');

// Keep the host loader/glibc as the OS ABI. Ship all other linked libraries.
const osLibraries = /^(?:ld-linux[^/]*|lib(?:c|m|dl|rt|pthread|resolv|util|anl)\.so(?:\..*)?)$/;
const dependencies = new Map();
for (let i = 0; i < elfFiles.length; i++) {
  const output = run('ldd', [elfFiles[i]]);
  if (output.includes('not found')) throw new Error('Unresolved build-host library: ' + elfFiles[i]);
  for (const match of output.matchAll(/^\s*(\S+)\s+=>\s+(\/\S+)/gm)) {
    const [, name, source] = match;
    if (osLibraries.test(name)) continue;
    if (dependencies.has(name)) {
      if (hash(dependencies.get(name)) !== hash(source)) throw new Error('Conflicting runtime libraries: ' + name);
      continue;
    }
    dependencies.set(name, source);
    const destination = join(libraries, name);
    if (!existsSync(destination)) { copy(source, destination, true); elfFiles.push(destination); }
  }
}

const systemPackages = new Map();
for (const source of [...dependencies.values(), ...['jq', 'curl', 'openssl', 'awk', 'flock'].map(name => run('bash', ['-c', 'command -v "$1"', 'bash', name])), realpathSync(certFile)]) {
  if (existsSync('/usr/bin/rpm')) {
    const [name, version, license, sourceRpm, url] = run('rpm', ['-qf', '--qf', '%{NAME}\n%{VERSION}-%{RELEASE}\n%{LICENSE}\n%{SOURCERPM}\n%{URL}\n', realpathSync(source)]).split('\n');
    if (systemPackages.has(name)) continue;
    const sourceName = sourceRpm.replace(/-[0-9].*$/, '');
    const aliases = { 'libstdc++': 'libgcc' };
    const candidates = [...new Set([name, sourceName, aliases[name]])].filter(Boolean);
    let noticeCount = 0;
    for (const candidate of candidates) {
      const directory = join('/usr/share/licenses', candidate);
      if (existsSync(directory)) {
        cpSync(directory, join(notices, 'system', candidate), { recursive: true, dereference: true });
        noticeCount++;
      }
      const docDirectory = join('/usr/share/doc', candidate);
      if (existsSync(docDirectory)) {
        for (const file of readdirSync(docDirectory).filter(file => /^(?:LICEN[CS]E|COPYING|COPYRIGHT|NOTICE)(?:$|[._-])/i.test(file))) {
          if (statSync(join(docDirectory, file)).isFile()) {
            copy(join(docDirectory, file), join(notices, 'system', candidate, file));
            noticeCount++;
          }
        }
      }
    }
    if (name === 'zlib') {
      const header = readFileSync('/usr/include/zlib.h', 'utf8');
      mkdirSync(join(notices, 'system/zlib'), { recursive: true });
      writeFileSync(join(notices, 'system/zlib/LICENSE'), header.slice(0, header.indexOf('*/') + 2) + '\n');
      noticeCount++;
    }
    // Some openEuler RPMs omit the upstream notice. Retrieve only the
    // pinned build-time supplement; no license download runs in production.
    if (!noticeCount && name === 'krb5-libs') {
      const notice = execFileSync('curl', ['--fail', '--silent', '--show-error',
        '--location', '--proto', '=https', '--max-time', '60',
        'https://raw.githubusercontent.com/krb5/krb5/krb5-1.21.2-final/NOTICE']);
      if (createHash('sha256').update(notice).digest('hex') !==
          '0d5373486138cb176c063db98274b4c4ab6ef3518c4191360736384b780306c2') {
        throw new Error('Kerberos license notice checksum mismatch');
      }
      mkdirSync(join(notices, 'system', name), { recursive: true });
      writeFileSync(join(notices, 'system', name, 'NOTICE'), notice);
      noticeCount++;
    }
    if (license === 'Public Domain') {
      mkdirSync(join(notices, 'system', name), { recursive: true });
      writeFileSync(join(notices, 'system', name, 'NOTICE'), 'The openEuler package metadata declares this component Public Domain.\nSource: ' + url + '\nSource RPM: ' + sourceRpm + '\n');
      noticeCount++;
    }
    if (!noticeCount) throw new Error('No license notice found for bundled RPM: ' + name);
    systemPackages.set(name, { name, version, license, source_rpm: sourceRpm, url });
  } else {
    const owner = run('dpkg-query', ['-S', realpathSync(source)]).split(': ')[0];
    const name = owner.split(':')[0];
    if (systemPackages.has(name)) continue;
    const version = run('dpkg-query', ['-W', '-f=' + '$' + '{Version}', owner]);
    copy(join('/usr/share/doc', name, 'copyright'), join(notices, 'system', name, 'copyright'));
    systemPackages.set(name, { name, version });
  }
}
json(join(notices, 'system-packages.json'), [...systemPackages.values()]);

const cargo = JSON.parse(run('cargo', ['metadata', '--locked', '--offline', '--format-version', '1']));
const crates = [];
for (const pkg of cargo.packages.filter(pkg => pkg.source)) {
  const directory = dirname(pkg.manifest_path);
  const names = readdirSync(directory).filter(name => /^(?:LICENSE|LICENCE|COPYING|NOTICE)(?:$|[._-])/i.test(name) && statSync(join(directory, name)).isFile());
  if (pkg.license_file && !names.includes(pkg.license_file)) names.push(pkg.license_file);
  for (const name of names) copy(join(directory, name), join(notices, 'rust', pkg.name + '-' + pkg.version, name));
  crates.push({ name: pkg.name, version: pkg.version, license: pkg.license, repository: pkg.repository });
}
json(join(notices, 'rust-crates.json'), crates);

let minimumGlibc = '2.34';
const newer = (left, right) => {
  const a = left.split('.').map(Number), b = right.split('.').map(Number);
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) > (b[i] ?? 0);
  }
  return false;
};
for (const path of elfFiles) {
  const info = run('readelf', ['--version-info', path]);
  for (const match of info.matchAll(/Name: GLIBC_([0-9.]+)/g)) if (newer(match[1], minimumGlibc)) minimumGlibc = match[1];
  const rpath = '$ORIGIN/' + relative(dirname(path), libraries);
  run(flags.patchelf, ['--set-rpath', rpath, path]);
}
// Relocation changes ONNX Runtime's ELF bytes; record input and delivered hashes.
const gitFiles = run('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z', '.']).split('\0').filter(Boolean).sort();
const snapshot = createHash('sha256');
for (const name of gitFiles) if (existsSync(name) && statSync(name).isFile()) snapshot.update(name + '\0' + hash(name) + '\n');
const metadata = {
  format: 1, version: cargo.packages.find(pkg => pkg.name === 'censorprivacy').version,
  platform: { os: 'linux', distribution: readFileSync('/etc/os-release', 'utf8'), arch: 'x86_64', minimum_glibc: minimumGlibc, cpu: 'x86_64; ONNX Runtime CPU execution provider' },
  source: { base_commit: run('git', ['rev-parse', 'HEAD']), working_tree_dirty: Boolean(run('git', ['status', '--porcelain'])), snapshot_sha256: snapshot.digest('hex') },
  model: { id: manifest.model_id, manifest_sha256: hash(join(sourceModel, 'manifest.json')) },
  onnx_runtime: { input_sha256: hash(ort), delivered_sha256: hash(ortDestination), library: 'lib/censorprivacy/libonnxruntime.so' },
  tools, system_packages: [...systemPackages.keys()].sort(), rustc: run('rustc', ['--version']),
  patchelf: run(flags.patchelf, ['--version']),
  runtime_dependencies: ['Linux x86_64 kernel with /proc', 'glibc >= ' + minimumGlibc, 'Bash and GNU coreutils, grep, sed'],
  optional: { isolated_agent_runtime: 'privacy-run additionally requires root, iproute2, nftables, libseccomp and namespace support; not part of the gateway/TLS self-test' },
};
json(join(share, 'BUNDLE.json'), metadata);
const quickstart = readFileSync(join(root, 'scripts/release/README.md'), 'utf8')
  .replaceAll('@MIN_GLIBC@', minimumGlibc).replaceAll('@VERSION@', metadata.version);
writeFileSync(join(bundle, 'README.md'), quickstart);
writeFileSync(join(bundle, 'SHA256SUMS'), files(bundle).map(path => hash(path) + '  ' + relative(bundle, path)).join('\n') + '\n');
console.log(JSON.stringify({ files: files(bundle).length, minimum_glibc: minimumGlibc, source_snapshot: metadata.source.snapshot_sha256 }));
