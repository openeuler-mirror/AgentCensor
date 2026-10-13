#!/usr/bin/env bash
# Exercise boot planning and orchestration with mocks; never edit host boot files.
set -euo pipefail
test_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$test_dir/setup-agentcensor.sh"
bash -n "$test_dir/setup-agentcensor.sh"

[[ $(lsm_with_bpf 'lockdown,capability,selinux') == lockdown,capability,selinux,bpf ]]
[[ $(lsm_with_bpf 'capability,apparmor,bpf') == capability,apparmor,bpf ]]
if lsm_with_bpf 'selinux bad' >/dev/null; then exit 1; fi

# Default-kernel plan preserves the configured LSM list.
python3 - "$test_dir/setup-agentcensor.sh" <<'PY'
import subprocess, tempfile
from pathlib import Path
script = Path(__import__('sys').argv[1])
with tempfile.TemporaryDirectory(prefix='agentcensor-setup-test-') as directory:
    root = Path(directory)
    invocation = root / 'grubby-args'
    # Functions replace only system mutations; Python, shell quoting, boot
    # argument extraction, backup writes and the actual planner still execute.
    source = '''
source "$1"
grubby() {
  if [[ $1 == --info=DEFAULT ]]; then
    printf '%s\n' 'kernel="/boot/vmlinuz-test"' 'args="quiet lsm=lockdown,capability,selinux systemd.unified_cgroup_hierarchy=0"'
  else printf '%s\n' "$@" > "$2_path"; fi
}
mktemp() { printf '%s\n' "$3_path"; }
cp() { :; }
prepare_boot
'''.replace('$2_path', '${plan_path}').replace('$3_path', '${backup_path}')
    env = dict(__import__('os').environ, plan_path=str(invocation), backup_path=str(root))
    result = subprocess.run(['bash', '-c', source, 'bash', str(script)], env=env, text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    args = invocation.read_text().splitlines()
    assert args[0] == '--update-kernel=DEFAULT', args
    assert '--remove-args=systemd.unified_cgroup_hierarchy cgroup_no_v1 lsm' in args, args
    assert args[-1] == '--args=systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=lockdown,capability,selinux,bpf', args
    assert (root/'default-kernel.txt').exists()
    assert (root/'agentcensor-args.txt').exists()
PY

# Reboot-required preparation cannot silently proceed to native installation.
(
  require_root() { :; }
  check_kernel() { :; }
  ensure_fuse() { :; }
  ensure_securityfs() { :; }
  is_cgroup_v2() { return 1; }
  has_bpf_lsm() { return 1; }
  configure_boot=false
  if prepare >/dev/null 2>&1; then exit 1; else [[ $? == 10 ]]; fi
)

# Installation/start ordering, and no installation if preflight fails.
python3 - "$test_dir/setup-agentcensor.sh" <<'PY'
import os, subprocess, tempfile
from pathlib import Path
script = Path(__import__('sys').argv[1])
with tempfile.TemporaryDirectory(prefix='agentcensor-run-test-') as directory:
    root = Path(directory)
    for name in ['install-agentcensor.sh', 'run-atomic-code-change-demo.sh', 'install-dsh-censorpivot.sh']:
        (root/name).write_text('printf "%s\\n" "$(basename "$0") $*" >> "$setup_test_log"\n')
    env = dict(os.environ, setup_test_log=str(root/'calls'))
    common = '''
source "$1"
setup_dir=$2
require_root() { :; }
ensure_fuse() { :; }
ensure_securityfs() { :; }
systemctl() { return 1; }
'''
    failed = common + 'check_environment() { return 1; }; run_stack'
    result = subprocess.run(['bash','-c',failed,'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode != 0
    assert not (root/'calls').exists(), 'failed preflight invoked installer'
    passed = common + 'check_environment() { :; }; run_demo=true; run_stack'
    result = subprocess.run(['bash','-c',passed,'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode == 0, result.stderr
    assert (root/'calls').read_text().splitlines() == [
        'install-agentcensor.sh all', 'install-agentcensor.sh start',
        'install-agentcensor.sh doctor', 'run-atomic-code-change-demo.sh ']

    # DSH version/dependency checks must succeed before stopping services or
    # installing anything. Exercise the real shell check with a spaced path.
    (root/'calls').unlink()
    dsh_source = root/'DSH source'
    dsh_source.mkdir()
    (dsh_source/'package.json').write_text('{}')
    commands = root/'bin'
    commands.mkdir()
    for name, body in {
        'node': 'exit 0',
        'pnpm': 'printf "%s\\n" "$setup_test_dsh_version"',
    }.items():
        command = commands/name
        command.write_text('#!/usr/bin/env bash\n'+body+'\n')
        command.chmod(0o755)
    env.update(PATH=str(commands)+os.pathsep+env['PATH'], setup_test_dsh_root=str(dsh_source))
    with_dsh = common + '''
check_environment() { :; }
as_dsh_user() { "$@"; }
dsh_root=$setup_test_dsh_root
run_demo=true
run_stack
'''
    env['setup_test_dsh_version'] = '0.1.5-rc.20'
    result = subprocess.run(['bash','-c',with_dsh,'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode != 0, 'unsupported DSH version passed preflight'
    assert not (root/'calls').exists(), 'failed DSH preflight invoked installer'
    env['setup_test_dsh_version'] = '0.1.5-rc.2'
    result = subprocess.run(['bash','-c',with_dsh,'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode == 0, result.stderr
    assert (root/'calls').read_text().splitlines() == [
        'install-agentcensor.sh all', 'install-agentcensor.sh start',
        'install-agentcensor.sh doctor', 'run-atomic-code-change-demo.sh ',
        'install-dsh-censorpivot.sh ']
PY

printf 'AgentCensor setup boot and deployment checks passed.\n'
