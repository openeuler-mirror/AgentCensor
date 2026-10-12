#!/usr/bin/env bash
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/lib-cli.sh"

# Failure checkpoint: report script/line and dump the daemon log tail.
on_err() {
    local rc=$?
    echo "[FAIL] $(basename "$0"):${1} rc=${rc}" >&2
    for d in "${runtime_dir:-}" "${lab_dir:-}"; do
        if [[ -n ${d} && -f ${d}/daemon.log ]]; then
            echo "--- ${d}/daemon.log (last 30 lines) ---" >&2
            tail -30 "${d}/daemon.log" >&2
        fi
    done
}
trap 'on_err "${LINENO}"' ERR
set -euo pipefail

# 断言依赖系统工具的英文输出 (id(1) strerror(3) 等会被 locale 本地化),
# 统一固定 C locale, 避免 zh_CN 环境下 grep 误判。
export LC_ALL=C

# Failure checkpoint: report script/line and dump the daemon log tail.
on_err() {
    local rc=$?
    echo "[FAIL] $(basename "$0"):${1} rc=${rc}" >&2
    for d in "${runtime_dir:-}" "${lab_dir:-}"; do
        if [[ -n ${d} && -f ${d}/daemon.log ]]; then
            echo "--- ${d}/daemon.log (last 30 lines) ---" >&2
            tail -30 "${d}/daemon.log" >&2
        fi
    done
}
trap 'on_err "${LINENO}"' ERR

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
runtime_dir=$(mktemp -d /tmp/censorguard-install.XXXXXX)
stage="${runtime_dir}/root"
dist_dir="${runtime_dir}/dist"

cleanup() {
    case ${runtime_dir} in
        /tmp/censorguard-install.*) rm -rf -- "${runtime_dir}" ;;
        *) echo "refusing to remove unexpected test path: ${runtime_dir}" >&2 ;;
    esac
}
trap cleanup EXIT

for tool in systemd-analyze systemd-sysusers systemd-tmpfiles tar sha256sum; do
    command -v "${tool}" >/dev/null
done

make --no-print-directory -C "${repo_dir}" install-files DESTDIR="${stage}"

declare -A expected_modes=(
    [usr/sbin/censorguardd]=755
    [usr/bin/censorguardctl]=755
    [usr/bin/censorguard-grpc]=755
    [usr/lib/censorguard/enforce.bpf.o]=644
    [etc/censorguard/base.yaml]=640
    [etc/censorguard/standard-dev.yaml]=640
    [etc/censorguard/high-priv.yaml]=640
    [usr/lib/systemd/system/censorguardd.service]=644
    [usr/lib/sysusers.d/censorguard.conf]=644
    [usr/lib/tmpfiles.d/censorguard.conf]=644
    [usr/share/doc/censorguard/README.md]=644
    [usr/share/licenses/censorguard/LICENSE]=644
)
for path in "${!expected_modes[@]}"; do
    [[ -f ${stage}/${path} ]]
    [[ $(stat -c '%a' "${stage}/${path}") == "${expected_modes[${path}]}" ]]
done

# An upgrade must never replace the administrator's active policy.
printf 'administrator-owned-policy\n' >"${stage}/etc/censorguard/base.yaml"
make --no-print-directory -C "${repo_dir}" install-files DESTDIR="${stage}" \
    >"${runtime_dir}/reinstall.log"
grep -q '^administrator-owned-policy$' "${stage}/etc/censorguard/base.yaml"
grep -q 'preserving existing' "${runtime_dir}/reinstall.log"

systemd-sysusers --dry-run --root="${stage}" \
    "${stage}/usr/lib/sysusers.d/censorguard.conf" \
    >"${runtime_dir}/sysusers.log" 2>&1
grep -qi 'censorguard' "${runtime_dir}/sysusers.log"
systemd-tmpfiles --create --graceful --root="${stage}" \
    "${stage}/usr/lib/tmpfiles.d/censorguard.conf"
# The staged package intentionally does not contain the operating system's base units or /bin/kill.
# Add minimal verifier-only fixtures so --root checks this unit instead of the host installation.
install -d -m 0755 "${stage}/bin"
install -m 0755 /bin/kill "${stage}/bin/kill"
for target in basic.target multi-user.target network-online.target shutdown.target sysinit.target; do
    printf '[Unit]\nDefaultDependencies=no\n' \
        >"${stage}/usr/lib/systemd/system/${target}"
done
systemd-analyze verify --root="${stage}" censorguardd.service

# Restore a valid policy and execute the staged daemon's read-only configuration gate.
install -m 0640 "${repo_dir}/config/base.yaml" \
    "${stage}/etc/censorguard/base.yaml"
write_test_config "${runtime_dir}/config.yaml" "${stage}/etc/censorguard/base.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}"
"${stage}/usr/sbin/censorguardd" launch \
    --config "${runtime_dir}/config.yaml" --check-config \
    >"${runtime_dir}/check-config.log"
grep -q 'configuration is valid' "${runtime_dir}/check-config.log"
write_test_config "${runtime_dir}/bad-config.yaml" "${stage}/etc/censorguard/base.yaml" "${repo_dir}/bpf/enforce.bpf.o" "${runtime_dir}/bad" "${runtime_dir}/bad-state" censorguard-group-that-does-not-exist
set +e
"${stage}/usr/sbin/censorguardd" launch \
    --config "${runtime_dir}/bad-config.yaml" --duration 1 \
    >"${runtime_dir}/bad-group.log" 2>&1
bad_group_rc=$?
set -e
[[ ${bad_group_rc} -ne 0 ]]
grep -q '^Error:' "${runtime_dir}/bad-group.log"
[[ ! -e ${runtime_dir}/bad/ctl.sock ]]
[[ ! -e ${runtime_dir}/bad/events.sock ]]

make --no-print-directory -C "${repo_dir}" package DISTDIR="${dist_dir}"
archive=$(find "${dist_dir}" -maxdepth 1 -type f -name 'Censorguard-*.tar.gz' -print -quit)
[[ -n ${archive} ]]
(cd -- "${dist_dir}" && sha256sum -c "$(basename -- "${archive}").sha256")
first_checksum=$(sha256sum "${archive}" | awk '{ print $1 }')
make --no-print-directory -C "${repo_dir}" package DISTDIR="${dist_dir}" \
    >"${runtime_dir}/second-package.log"
second_checksum=$(sha256sum "${archive}" | awk '{ print $1 }')
[[ ${first_checksum} == "${second_checksum}" ]]
tar -tzf "${archive}" >"${runtime_dir}/archive.list"
for path in ./usr/sbin/censorguardd ./usr/bin/censorguardctl ./usr/bin/censorguard-grpc \
    ./usr/lib/censorguard/enforce.bpf.o ./etc/censorguard/base.yaml.example \
    ./usr/lib/systemd/system/censorguardd.service; do
    grep -Fxq "${path}" "${runtime_dir}/archive.list"
done
if grep -Fxq './etc/censorguard/base.yaml' "${runtime_dir}/archive.list"; then
    echo "tar package would overwrite the active policy" >&2
    exit 1
fi

make --no-print-directory -C "${repo_dir}" uninstall DESTDIR="${stage}"
[[ ! -e ${stage}/usr/sbin/censorguardd ]]
[[ ! -e ${stage}/usr/bin/censorguardctl ]]
[[ ! -e ${stage}/usr/bin/censorguard-grpc ]]
[[ ! -e ${stage}/usr/lib/systemd/system/censorguardd.service ]]
[[ ! -e ${stage}/usr/share/doc/censorguard ]]
grep -q '^rules:' "${stage}/etc/censorguard/base.yaml"

echo "deployment integration passed: layout, preservation, unit, package and uninstall"
