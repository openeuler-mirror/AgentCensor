#!/usr/bin/env bash
SCRIPT_DIR=${BASH_SOURCE[0]%/*}
[[ "$SCRIPT_DIR" != "${BASH_SOURCE[0]}" ]] || SCRIPT_DIR=.
SCRIPT_DIR=$(cd -- "$SCRIPT_DIR" 2>/dev/null && pwd -P) || exit 3
source "$SCRIPT_DIR/check-deployment-env-common.sh"
source "$SCRIPT_DIR/check-deployment-env-system.sh"
source "$SCRIPT_DIR/check-deployment-env-artifacts.sh"
finish_report() {
    local conclusion
    if ((BLOCKS > 0)); then
        conclusion='发现阻断性缺失，需按下列方法处理后再部署。'
    elif ((UNKNOWNS > 0)); then
        conclusion='存在待确认事项，需补充证据；不能将信息不足当作兼容性通过。'
    elif ((MISSING > 0)); then
        conclusion='存在随包 RPM 可补齐的缺失，需核验并安装相应依赖后复查。'
    else
        conclusion='静态检查未发现阻断性缺失，后续部署及运行验收参见部署指南。'
    fi
    printf '\n检查完成：采集=%d，已确认阻断=%d，随包可补齐=%d，关键待确认=%d\n' \
        "$COUNT" "$BLOCKS" "$MISSING" "$UNKNOWNS" >> "$OUT/deployment-environment-report.txt" || WRITE_ERROR=1
    {
        printf 'CensorGuard 缺失情况与处理方法报告\n架构：%s\n运行内核：%s\n' "$MACHINE" "$RELEASE"
        printf '已确认阻断=%d；随包可补齐=%d；关键待确认=%d\n结论：%s\n' "$BLOCKS" "$MISSING" "$UNKNOWNS" "$conclusion"
        printf '\n标记：BLOCK=阻断性缺失；MISSING=随包有候选依赖；UNKNOWN=信息不足；WARN=需复核。\n'
        printf '所有命令均为操作人员参考，按部署方确认的变更流程执行。内核/LSM 变更如需重启，应另行安排维护窗口。\n'
        printf 'RPM 元数据命中仅说明有候选包，签名、传递依赖、安装事务和安装后的 ABI 需复核。\n'
        printf '静态检查覆盖本机可读证据；BPF 加载、verifier 和业务性能通过运行验收确认。\n'
        printf '以下“可直接执行命令”使用本次 --rpm-dir 指定的离线仓库；先执行预览命令，确认事务后再执行安装命令。\n'
    } > "$OUT/deployment-operation-report.txt" || WRITE_ERROR=1
    append_text "$OUT/raw/actions.txt" "$OUT/deployment-operation-report.txt"
    if ((WRITE_ERROR)); then printf '%s\n' '报告写入失败，已采集文件保留。' >&2; return 3; fi
    rm -r -- "$OUT/raw" || { printf '%s\n' '检查临时文件清理失败，报告已保留。' >&2; return 3; }
    if [[ "$REPLACED_PREVIOUS" == true ]]; then
        printf '已替换该目录中上一轮报告。\n'
        if [[ "$KEEP_PREVIOUS" == true ]]; then printf '上一轮报告保留在：%s/previous/\n' "$OUT"; fi
    fi
    printf '\n检查完成。部署环境报告：%s/deployment-environment-report.txt\n缺失及处理报告：%s/deployment-operation-report.txt\n' "$OUT" "$OUT"
    printf '机器可读结论：%s\n' "$FINDINGS_FILE"
    return 0
}

main() {
    local requested='' kit=${KIT:-}
    ARCH=aarch64 TIMEOUT=20 DAEMON='' CTL='' BPF_OBJECT='' CONFIG='' RPM_DIR=''
    MACHINE=unknown RELEASE=unknown CACHE_OK=false BUNDLE_COMPLETE=false
    while (($#)); do
        case "$1" in
            --help|-h) usage; return 0 ;;
            --keep-previous) KEEP_PREVIOUS=true; shift ;;
            --output|--arch|--daemon|--ctl|--bpf-object|--config|--rpm-dir|--timeout|--kit|--findings)
                if (($# < 2)) || [[ -z "$2" ]]; then printf '缺少参数：%s\n' "$1" >&2; return 2; fi
                case "$1" in
                    --output) requested=$2 ;; --arch) ARCH=$2 ;; --daemon) DAEMON=$2 ;; --ctl) CTL=$2 ;;
                    --bpf-object) BPF_OBJECT=$2 ;; --config) CONFIG=$2 ;; --rpm-dir) RPM_DIR=$2 ;;
                    --timeout) TIMEOUT=$2 ;; --kit) kit=$2 ;; --findings) FINDINGS_FILE=$2 ;;
                esac
                shift 2 ;;
            *) printf '未知参数：%s\n' "$1" >&2; usage >&2; return 2 ;;
        esac
    done
    case "$ARCH" in aarch64|x86_64|any) ;; *) printf '%s\n' '无效 --arch' >&2; return 2 ;; esac
    if [[ ! "$TIMEOUT" =~ ^[1-9][0-9]{0,2}$ ]] || ((10#$TIMEOUT > 300)); then printf '%s\n' '--timeout 必须为 1..300 的整数秒。' >&2; return 2; fi
    if [[ -z "$requested" ]]; then
        printf '%s\n' '缺少 --output：报告输出目录必须由操作人员指定，脚本不设默认路径。' \
            '例如：bash check-deployment-env.sh --output /var/tmp/cg-deployment-check' >&2
        return 2
    fi
    if [[ -n "$RPM_DIR" && "$RPM_DIR" != /* ]]; then RPM_DIR="$PWD/$RPM_DIR"; fi
    if [[ -n "$FINDINGS_FILE" && "$FINDINGS_FILE" != /* ]]; then FINDINGS_FILE="$PWD/$FINDINGS_FILE"; fi
    if [[ -n "$kit" ]]; then
        [[ -d "$kit" ]] || { printf '交付包根目录不存在：%s\n' "$kit" >&2; return 2; }
        CG_KIT_ROOT=$(cd -- "$kit" && pwd -P)
    else
        CG_KIT_ROOT=$(kit_root) || CG_KIT_ROOT=$PWD
    fi
    discover_inputs
    init_report "$requested" || return 3
    report_inputs
    check_identity
    check_bundle
    check_tools
    check_kernel
    check_storage
    check_artifacts
    finish_report
}

declare -a LSM_HOOKS=(file_open file_permission path_truncate path_unlink path_rmdir path_rename file_truncate path_link path_symlink path_mkdir path_chmod path_chown inode_setxattr inode_removexattr inode_set_acl path_mknod mmap_file file_mprotect bprm_check_security socket_connect socket_bind socket_sendmsg socket_listen socket_accept inet_conn_request socket_recvmsg inode_free_security task_kill ptrace_access_check ptrace_traceme bpf)
declare -A ELF_FILES=() ELF_RAW=() ELF_CLASS=() ELF_ENDIAN=() ELF_MACHINE=() ELF_TYPE=() ELF_MOUNTS=() LIBRARIES=() BUNDLE_PACKAGES=() BUNDLE_PROVIDES=() BUNDLE_PACKAGE_NAMES=()
declare -A INPUT_SOURCE=() INPUT_SEARCHED=()
CG_KIT_ROOT=${CG_KIT_ROOT:-}
KEEP_PREVIOUS=${KEEP_PREVIOUS:-false}
REPLACED_PREVIOUS=${REPLACED_PREVIOUS:-false}
AUTO_PATH=''
FINDINGS_FILE=${FINDINGS_FILE:-}
FINDING_CANDIDATE=''
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; exit "$?"; fi
