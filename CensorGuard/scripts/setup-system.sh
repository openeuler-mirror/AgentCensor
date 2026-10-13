#!/usr/bin/env bash
# Install already-built artifacts. Build as the invoking user; elevate only deployment.
set -Eeuo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if (( $# )); then
    printf 'usage: bash scripts/setup-system.sh\n' >&2
    exit 2
fi
if (( EUID != 0 )); then
    exec sudo bash "${BASH_SOURCE[0]}"
fi
for tool in make systemctl systemd-sysusers systemd-tmpfiles; do
    command -v "${tool}" >/dev/null
done
for artifact in target/release/censorguardd target/release/censorguardctl target/release/censorguard-grpc bpf/enforce.bpf.o; do
    [[ -s ${repo_dir}/${artifact} ]] || { printf 'Missing %s; run make all first.\n' "${artifact}" >&2; exit 2; }
done
[[ -r /sys/kernel/btf/vmlinux ]] || { printf 'Kernel BTF is unavailable.\n' >&2; exit 2; }
grep -qw bpf /sys/kernel/security/lsm || { printf 'Enable BPF LSM in the kernel boot parameters first.\n' >&2; exit 2; }
systemctl show --property=Version >/dev/null
config_path=/etc/censorguard/base.yaml
if [[ ! -e ${config_path} ]]; then config_path="${repo_dir}/config/base.yaml"; fi
# Reject an obsolete/invalid public configuration before interrupting an existing service.
"${repo_dir}/target/release/censorguardd" launch --config "${config_path}" --check-config

printf '[1/4] Stopping the existing service (if installed)\n'
if [[ $(systemctl show censorguardd.service --property=LoadState --value) != not-found ]]; then
    systemctl stop censorguardd.service
fi
printf '[2/4] Installing binaries, BPF, configuration and service files\n'
make --no-print-directory -C "${repo_dir}" install-files
systemd-sysusers /usr/lib/sysusers.d/censorguard.conf
systemd-tmpfiles --create /usr/lib/tmpfiles.d/censorguard.conf
printf '[3/4] Enabling and starting censorguardd\n'
systemctl daemon-reload
systemctl enable --now censorguardd.service
printf '[4/4] Checking readiness\n'
# Type=exec becomes active before BPF/grpc initialization has finished.
ctl="${repo_dir}/target/release/censorguardctl"
for attempt in {1..30}; do
    if "${ctl}" doctor >/dev/null 2>&1; then
        "${ctl}" status
        "${ctl}" doctor
        printf '\nCensorGuard installation complete. DSH profile setup: docs/DSH插件指南.md\n'
        exit 0
    fi
    sleep 1
done
"${ctl}" doctor || true
journalctl -u censorguardd.service -n 30 --no-pager >&2
exit 1
