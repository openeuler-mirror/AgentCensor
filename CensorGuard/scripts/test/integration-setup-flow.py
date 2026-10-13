#!/usr/bin/env python3
"""Check deployment ordering with mocked system tools; never touch a real service."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[2]
if os.geteuid() != 0:
    raise SystemExit('Run as root: this test mocks the privileged deployment branch.')


with tempfile.TemporaryDirectory(prefix='cg-setup-flow-') as temporary:
    directory = Path(temporary)
    project = directory / 'project'
    tools = directory / 'tools'
    (project / 'scripts').mkdir(parents=True)
    (project / 'target/release').mkdir(parents=True)
    (project / 'bpf').mkdir()
    (project / 'config').mkdir()
    (project / 'config/base.yaml').write_text('rules: []\n')
    tools.mkdir()
    shutil.copyfile(REPO / 'scripts/setup-system.sh', project / 'scripts/setup-system.sh')
    log = directory / 'calls.log'
    counter = directory / 'attempts'
    env = {**os.environ, 'PATH': str(tools) + os.pathsep + os.environ['PATH'],
           'CG_TEST_LOG': str(log), 'CG_TEST_COUNTER': str(counter)}

    def script(path, body):
        path.write_text('#!/usr/bin/env bash\nset -eu\n' + body)
        path.chmod(0o755)

    script(tools / 'systemctl', '''printf 'systemctl %s\n' "$*" >>"$CG_TEST_LOG"
if [[ "$*" == *LoadState* ]]; then printf 'loaded\n'; fi
''')
    for tool in ('make', 'systemd-sysusers', 'systemd-tmpfiles', 'journalctl', 'sleep'):
        script(tools / tool, f'printf "{tool} %s\\n" "$*" >>"$CG_TEST_LOG"\n')
    script(tools / 'grep', 'exit 0\n')  # Kernel support is a separate runtime check.
    script(project / 'target/release/censorguardd', '''printf 'config-check\n' >>"$CG_TEST_LOG"
exit "${CG_TEST_BAD_CONFIG:-0}"
''')
    script(project / 'target/release/censorguardctl', '''printf 'ctl %s\n' "$*" >>"$CG_TEST_LOG"
if [[ "$1" == doctor ]]; then
  attempts=0; if [[ -f "$CG_TEST_COUNTER" ]]; then read -r attempts <"$CG_TEST_COUNTER"; fi
  printf '%s\n' "$((attempts + 1))" >"$CG_TEST_COUNTER"
  if [[ "$attempts" -lt 2 || "${CG_TEST_START_FAIL:-0}" == 1 ]]; then exit 1; fi
fi
''')
    script(project / 'target/release/censorguard-grpc', 'exit 0\n')
    (project / 'bpf/enforce.bpf.o').write_bytes(b'fixture')

    def run(**overrides):
        log.write_text('')
        counter.unlink(missing_ok=True)
        result = subprocess.run(['bash', str(project / 'scripts/setup-system.sh')],
                                env={**env, **overrides}, text=True, capture_output=True, timeout=10)
        return result, log.read_text().splitlines()

    result, calls = run(CG_TEST_BAD_CONFIG='1')
    assert result.returncode != 0
    assert 'config-check' in calls
    assert not any(' stop ' in call or call.startswith('make ') for call in calls), calls
    print('PASS invalid configuration cannot stop the existing service')

    result, calls = run()
    assert result.returncode == 0, result.stdout + result.stderr
    stop = calls.index('systemctl stop censorguardd.service')
    install = next(i for i, call in enumerate(calls) if call.startswith('make '))
    reload = calls.index('systemctl daemon-reload')
    start = calls.index('systemctl enable --now censorguardd.service')
    assert calls.index('config-check') < stop < install < reload < start
    assert calls.index('systemd-sysusers /usr/lib/sysusers.d/censorguard.conf') < start
    assert calls.index('systemd-tmpfiles --create /usr/lib/tmpfiles.d/censorguard.conf') < start
    assert calls.count('sleep 1') == 2 and calls.count('ctl status') == 1
    assert 'installation complete' in result.stdout
    print('PASS upgrade order and delayed readiness before reporting success')

    result, calls = run(CG_TEST_START_FAIL='1')
    assert result.returncode != 0
    assert 'installation complete' not in result.stdout
    assert any(call.startswith('journalctl ') for call in calls)
    print('PASS startup failure returns nonzero with diagnostics')
