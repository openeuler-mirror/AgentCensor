#!/usr/bin/env bash
# Exercise boot planning and orchestration with mocks; never edit host boot files.
set -euo pipefail
test_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$test_dir/setup-agentcensor.sh"
bash -n "$test_dir/setup-agentcensor.sh"

[[ $(lsm_with_bpf 'lockdown,capability,selinux') == lockdown,capability,selinux,bpf ]]
[[ $(lsm_with_bpf 'capability,apparmor,bpf') == capability,apparmor,bpf ]]
if lsm_with_bpf 'selinux bad' >/dev/null; then exit 1; fi
boot_args_ready <<< 'quiet systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=capability,bpf'
if boot_args_ready <<< 'quiet systemd.unified_cgroup_hierarchy=0 lsm=capability,bpf'; then exit 1; fi
if boot_args_ready <<< 'systemd.unified_cgroup_hierarchy=0 systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=capability,bpf'; then exit 1; fi
if boot_args_ready 'capability,selinux,bpf' <<< 'systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=capability,bpf'; then exit 1; fi

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
    if [[ -e "$2_path" && ${setup_test_drop_update:-false} == false ]]; then
      printf '%s\n' 'kernel="/boot/vmlinuz-test"' 'args="quiet lsm=lockdown,capability,selinux,bpf systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all"'
    else
      printf '%s\n' 'kernel="/boot/vmlinuz-test"' 'args="quiet lsm=lockdown,capability,selinux systemd.unified_cgroup_hierarchy=0"'
    fi
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
    assert not any(arg.startswith('--remove-args') for arg in args), args
    assert args[-1] == '--args=systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=lockdown,capability,selinux,bpf', args
    assert (root/'default-kernel.txt').exists()
    assert (root/'agentcensor-args.txt').exists()

    # A successful grubby exit without persisted arguments must not produce a
    # success/reboot message, including when prepare is used by an outer shell
    # conditional (where Bash can disable errexit inside functions).
    for invocation_source in [source, source.replace('prepare_boot\n', 'if prepare_boot; then exit 0; else exit $?; fi\n')]:
        env['setup_test_drop_update'] = 'true'
        result = subprocess.run(['bash', '-c', invocation_source, 'bash', str(script)], env=env, text=True, capture_output=True)
        assert result.returncode == 1, (result.stdout, result.stderr)
        assert 'did not persist' in result.stderr
        assert 'boot arguments verified' not in result.stdout
        assert 'REBOOT REQUIRED' not in result.stdout
    env.pop('setup_test_drop_update')

    invocation.unlink()
    already_prepared = source.replace(
        'args="quiet lsm=lockdown,capability,selinux systemd.unified_cgroup_hierarchy=0"',
        'args="quiet lsm=lockdown,capability,selinux,bpf systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all"')
    result = subprocess.run(['bash', '-c', already_prepared, 'bash', str(script)], env=env, text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    assert not invocation.exists(), 'already configured kernel was rewritten'
    assert 'already configured; no boot files changed' in result.stdout

    # openEuler grubby prints the kernel field without quotes.
    unquoted = already_prepared.replace('kernel="/boot/vmlinuz-test"', 'kernel=/boot/vmlinuz-test')
    result = subprocess.run(['bash', '-c', unquoted, 'bash', str(script)], env=env, text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    assert 'default boot kernel: /boot/vmlinuz-test' in result.stdout
PY

# Use the installed grubby against a complete temporary GRUB fixture. No /boot
# files or real grubenv are read or written. This catches actual argument
# replacement behavior that a mock of grubby's exit code cannot verify.
python3 - "$test_dir/setup-agentcensor.sh" <<'PY'
import shutil, subprocess, sys, tempfile
from pathlib import Path
binary = shutil.which('grubby')
if not binary:
    print('SKIP real grubby fixture (grubby not installed)')
    raise SystemExit(0)
with tempfile.TemporaryDirectory(prefix='agentcensor-grubby-test-') as directory:
    root = Path(directory)
    config = root/'grub.cfg'
    other_entry = '''menuentry 'other kernel' --id other {
 linux /boot/vmlinuz-other root=/dev/test quiet
 initrd /boot/initramfs-other.img
}
'''
    config.write_text('''set default="0"
menuentry 'test kernel' --id test {
 linux /boot/vmlinuz-test root=/dev/test quiet systemd.unified_cgroup_hierarchy=0 cgroup_no_v1=none lsm=lockdown,capability,selinux
 initrd /boot/initramfs-test.img
}
''' + other_entry)
    environment = root/'grubenv'
    contents = '# GRUB Environment Block\nsaved_entry=0\n'
    environment.write_text(contents + '#'*(1024-len(contents)))
    backup = root/'backup'
    backup.mkdir()
    shell = '''
source "$1"
fixture_config=$2
fixture_env=$3
fixture_backup=$4
grubby_binary=$5
grubby() { command "$grubby_binary" --grub2 --config-file="$fixture_config" --env="$fixture_env" --bad-image-okay --boot-filesystem=/ "$@"; }
mktemp() { printf '%s\n' "$fixture_backup"; }
cp() { :; }
prepare_boot
'''
    command = ['bash', '-c', shell, 'bash', sys.argv[1], str(config), str(environment), str(backup), binary]
    result = subprocess.run(command, text=True, capture_output=True)
    assert result.returncode == 0, (result.stdout, result.stderr)
    updated = config.read_text()
    assert 'systemd.unified_cgroup_hierarchy=1 cgroup_no_v1=all lsm=lockdown,capability,selinux,bpf' in updated, updated
    assert 'systemd.unified_cgroup_hierarchy=0' not in updated
    assert updated.count('systemd.unified_cgroup_hierarchy=') == 1
    assert updated.count('cgroup_no_v1=') == 1
    assert updated.count('lsm=') == 1
    assert other_entry in updated, 'nondefault kernel entry was changed'
    assert 'root=/dev/test quiet' in updated, 'unrelated boot arguments were lost'
    assert 'boot arguments verified' in result.stdout, result.stdout
    before = config.stat().st_mtime_ns
    result = subprocess.run(command, text=True, capture_output=True)
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert 'already configured; no boot files changed' in result.stdout
    assert config.stat().st_mtime_ns == before
    print('PASS real grubby: default entry updated, old values replaced and reread verified')
PY

# Inspection must not imply that unchanged boot settings will take effect.
(
  require_root() { :; }
  check_kernel() { :; }
  ensure_fuse() { :; }
  ensure_securityfs() { :; }
  is_cgroup_v2() { return 1; }
  has_bpf_lsm() { return 1; }
  configure_boot=false
  set +e
  output=$(prepare 2>&1)
  result=$?
  set -e
  [[ $result == 1 && $output != *'[REBOOT REQUIRED]'* && $output == *'boot settings were not changed'* ]]
  configure_boot=true
  systemd-detect-virt() { return 1; }
  boot_args_ready() { return 1; }
  prepare_boot() { :; }
  set +e
  output=$(prepare 2>&1)
  result=$?
  set -e
  [[ $result == 10 && $output == *'[REBOOT REQUIRED]'* ]]

  boot_args_ready() { return 0; }
  set +e
  output=$(prepare 2>&1)
  result=$?
  set -e
  [[ $result == 1 && $output != *'[REBOOT REQUIRED]'* && $output == *'instead of rebooting again'* ]]

  is_cgroup_v2() { return 0; }
  has_bpf_lsm() { return 0; }
  prepare_boot() { echo 'unexpected boot update' >&2; exit 1; }
  output=$(prepare 2>&1)
  [[ $output != *'[REBOOT REQUIRED]'* ]]
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

    # File installation uses build checks only, never loads FUSE or starts
    # services. Starting already installed files does not rebuild them.
    (root/'calls').unlink()
    install_only = common + '''
ensure_fuse() { echo 'unexpected FUSE setup' >&2; return 1; }
ensure_securityfs() { echo 'unexpected runtime setup' >&2; return 1; }
check_environment() { [[ $1 == build ]]; }
install_stack
'''
    result = subprocess.run(['bash','-c',install_only,'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode == 0, result.stderr
    assert (root/'calls').read_text().splitlines() == ['install-agentcensor.sh all']
    (root/'calls').unlink()
    start_only = common + 'check_environment() { [[ $1 == runtime ]]; }; start_stack'
    result = subprocess.run(['bash','-c',start_only,'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode == 0, result.stderr
    assert (root/'calls').read_text().splitlines() == ['install-agentcensor.sh start', 'install-agentcensor.sh doctor']

    (root/'calls').unlink()
    failed_start = common + 'check_environment() { return 1; }; start_stack'
    result = subprocess.run(['bash','-c',failed_start,'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode != 0
    assert not (root/'calls').exists(), 'failed runtime preflight started services'

    # DSH version/dependency checks must succeed before stopping services or
    # installing anything. Exercise the real shell check with a spaced path.
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

    (root/'calls').unlink()
    result = subprocess.run(['bash','-c',with_dsh.replace('run_demo=true\nrun_stack', 'install_stack'),'bash',str(script),str(root)],env=env,capture_output=True)
    assert result.returncode == 0, result.stderr
    assert (root/'calls').read_text().splitlines() == ['install-agentcensor.sh all', 'install-dsh-censorpivot.sh ']
PY

printf 'AgentCensor setup boot and deployment checks passed.\n'
