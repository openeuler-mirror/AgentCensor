#!/usr/bin/env bash
# CensorGuard 目标主机离线部署编排脚本
# 覆盖离线部署指南的：环境检查 -> 补齐运行依赖 -> 安装文件与初始配置 -> 启动并验证。
# 设计原则：只补齐"随包有候选 RPM"的缺失；内核与系统配置类缺失只输出开启方法与生效方式
set +e
set -u
set -o pipefail
export LC_ALL=C LANG=C SYSTEMD_PAGER=cat SYSTEMD_COLORS=0

SELF=${BASH_SOURCE[0]:-$0}
SCRIPT_DIR=$(cd -- "${SELF%/*}" 2>/dev/null && pwd -P) || SCRIPT_DIR=.

KIT=''
OUT=/var/tmp/cg-deploy-check
LOG=''
SUBCOMMAND=''
ASSUME_YES=false
DRY_RUN=false
SKIP_VERIFY=false
FORCE=false
CHECK_DIR=''
FINDINGS=''
STATE=''
SUMMARY=''

F_STATUS=(); F_KEY=(); F_REQUIRED=(); F_MSG=(); F_CAND=()
CAND_LIST=()
C_BLOCKS=0; C_UNKNOWNS=0; C_MISSINGS=0; C_PASS=0
INSTALLED_PKGS=()
RUN_TS=''

CONFIG_FILE=/etc/censorguard/base.yaml
DAEMON_BIN=/usr/sbin/censorguardd
CTL_BIN=/usr/bin/censorguardctl
UNIT=censorguardd.service
SOCKETS=(/run/censorguard/ctl.sock /run/censorguard/events.sock /run/censorguard/launch.sock)

usage() {
    cat <<'EOF'
用法：sudo bash cg-deploy.sh <子命令> [选项]

子命令：
  check      只做环境检查并生成报告，不改动本机任何配置
  fix        只补齐随包可提供的运行依赖，安装后自动复查
  install    安装文件与初始配置并启动服务（不设置开机启动）
  all        check -> fix -> 复查 -> install

选项：
  --kit 目录      交付包根目录，默认由脚本位置推导（<交付包>/tools/cg-deploy.sh）
  --out 目录      报告与日志目录，默认 /var/tmp/cg-deploy-check
  --log 文件      日志文件，默认 <out>/cg-deploy.log
  --yes           不再逐项交互确认（基础系统库升级仍需单独输入 yes）
  --dry-run       只打印将要执行的命令，不实际执行
  --skip-verify   跳过交付包 SHA256SUMS 校验
  --force         已安装或上轮检查未通过时仍继续（会先备份，风险自担）
  -h, --help      显示本帮助

退出码：
  0  成功，未发现阻断
  1  参数或用法错误
  2  存在阻断性缺失，需人工处理后再部署
  3  存在信息不足或材料缺失，需人工确认
  4  安装阶段失败

本脚本明确不做的事：
  1. 不执行 systemctl enable，不设置开机启动；
  2. 不修改内核启动参数、不写 sysctl、不做挂载 remount；
  3. 不 attach 业务进程、不发布策略、不加载阶段规则文件；
  4. 不在没有交互确认的情况下升级 glibc/systemd/kernel 等基础包。
EOF
}

log_write() {
    [[ -n "$LOG" ]] || return 0
    local ts
    printf -v ts '%(%Y-%m-%d %H:%M:%S)T' -1
    printf '%s %s\n' "$ts" "$*" >> "$LOG"
}

log_line() {
    printf '%s\n' "$*"
    log_write "$*"
}

log_step() { log_line "[步骤] $*"; }
log_info() { log_line "[信息] $*"; }
log_ok()   { log_line "[完成] $*"; }
log_warn() { log_line "[注意] $*"; }
log_err()  { printf '%s\n' "[错误] $*" >&2; log_write "[错误] $*"; }

die() {
    local code=$1
    shift
    log_err "$*"
    exit "$code"
}

# 打印并执行一条命令；--dry-run 时只打印。
run() {
    log_write "执行：$*"
    if [[ "$DRY_RUN" == true ]]; then
        printf '  [dry-run] %s\n' "$*"
        return 0
    fi
    "$@"
}

confirm() {
    local prompt=$1 reply
    if [[ "$ASSUME_YES" == true ]]; then
        return 0
    fi
    if [[ ! -t 0 ]]; then
        log_err "非交互环境，需要显式加 --yes 才能继续：$prompt"
        return 1
    fi
    read -r -p "$prompt [y/N] " reply
    case "$reply" in
        y|Y|yes|YES) return 0 ;;
        *) return 1 ;;
    esac
}

# 基础系统库变更需要逐字输入 yes，--yes 不豁免。
confirm_typed() {
    local prompt=$1 reply
    if [[ ! -t 0 ]]; then
        log_err "非交互环境无法确认基础系统库变更，已中止：$prompt"
        return 1
    fi
    read -r -p "$prompt 请逐字输入 yes 继续：" reply
    [[ "$reply" == yes ]]
}

# ---------------------------------------------------------------- 预检

resolve_kit() {
    if [[ -n "$KIT" ]]; then
        KIT=$(cd -- "$KIT" 2>/dev/null && pwd -P) || die 1 "交付包目录不存在：$KIT"
    else
        KIT=$(cd -- "$SCRIPT_DIR/.." 2>/dev/null && pwd -P) || die 1 "无法推导交付包根目录，请用 --kit 指定"
    fi
    local item
    for item in tools/check-deployment-env.sh release/CensorGuard-standalone-aarch64.tar.gz \
                release/rootfs rpms/runtime keys/RPM-GPG-KEY-openEuler; do
        [[ -e "$KIT/$item" ]] || die 1 "交付包不完整，缺少：$KIT/$item"
    done
    if ! grep -q -- '--findings' "$KIT/tools/check-deployment-env.sh"; then
        die 1 "交付包内的检查器不支持 --findings，请与本次部署脚本配套更新后再使用"
    fi
}

preflight() {
    RUN_TS=$(date '+%Y%m%dT%H%M%S')
    if ((BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 2))); then
        die 1 "需要 bash 4.2 或更高版本，当前 $BASH_VERSION"
    fi
    if [[ $EUID -ne 0 ]]; then
        die 1 "请以 root 执行：sudo bash $SELF $SUBCOMMAND"
    fi
    resolve_kit
    mkdir -p -- "$OUT/check" || die 1 "无法创建报告目录：$OUT"
    chmod 0700 -- "$OUT" 2>/dev/null || :
    [[ -n "$LOG" ]] || LOG="$OUT/cg-deploy.log"
    CHECK_DIR="$OUT/check"
    FINDINGS="$CHECK_DIR/findings.tsv"
    STATE="$OUT/state.env"
    SUMMARY="$OUT/deploy-summary.md"

    local machine
    machine=$(uname -m 2>/dev/null || echo unknown)
    log_info "交付包：$KIT"
    log_info "主机：$(uname -n 2>/dev/null || echo unknown)  架构：$machine  内核：$(uname -r 2>/dev/null || echo unknown)"
    log_info "报告目录：$OUT"
    [[ "$machine" == aarch64 ]] || log_warn "当前架构为 $machine，交付物为 aarch64；检查器会给出架构判定。"

    if [[ "$SKIP_VERIFY" == true ]]; then
        log_warn "已跳过交付包 SHA256SUMS 校验"
    elif [[ -f "$KIT/SHA256SUMS" ]]; then
        log_step "校验交付文件完整性（SHA256SUMS）"
        if ! ( cd -- "$KIT" && sha256sum -c --quiet SHA256SUMS ) >> "$LOG" 2>&1; then
            log_err "交付文件校验失败，详见 $LOG"
            die 1 "交付包完整性异常，停止部署"
        fi
        log_ok "交付文件校验通过"
    else
        log_warn "交付包内没有 SHA256SUMS，跳过校验"
    fi
}

# ---------------------------------------------------------------- 检查与分派

run_check() {
    local keep=$1 rc=0
    local -a args=(--output "$CHECK_DIR" --findings "$FINDINGS")
    [[ "$keep" == true ]] && args+=(--keep-previous)
    log_step "执行环境检查（只读采集，不修改本机）"
    # 检查本身是只读的，--dry-run 也要真实执行，否则拿不到分派依据。
    bash "$KIT/tools/check-deployment-env.sh" "${args[@]}" 2>&1 | tee -a "$LOG"
    rc=${PIPESTATUS[0]}
    if ((rc != 0)); then
        log_err "检查器返回 $rc（2=参数错误，3=报告目录或写入失败）"
        return "$rc"
    fi
    [[ -f "$FINDINGS" ]] || die 3 "检查器未生成结论清单：$FINDINGS"

    C_BLOCKS=0; C_UNKNOWNS=0; C_MISSINGS=0; C_PASS=0
    F_STATUS=(); F_KEY=(); F_REQUIRED=(); F_MSG=(); F_CAND=()
    local s k req msg cand
    while IFS=$'\t' read -r s k req msg cand || [[ -n "${s:-}" ]]; do
        [[ -z "${s:-}" || "$s" == \#* ]] && continue
        F_STATUS+=("$s"); F_KEY+=("$k"); F_REQUIRED+=("${req:-false}")
        F_MSG+=("${msg:-}"); F_CAND+=("${cand:-}")
        case "$s" in
            BLOCK) C_BLOCKS=$((C_BLOCKS + 1)) ;;
            MISSING) C_MISSINGS=$((C_MISSINGS + 1)) ;;
            UNKNOWN) [[ "${req:-false}" == true ]] && C_UNKNOWNS=$((C_UNKNOWNS + 1)) ;;
            PASS) C_PASS=$((C_PASS + 1)) ;;
        esac
    done < "$FINDINGS"

    {
        printf 'LAST_CHECK_TS="%s"\n' "$(date '+%Y-%m-%d %H:%M:%S')"
        printf 'BLOCKS=%d\n' "$C_BLOCKS"
        printf 'UNKNOWNS=%d\n' "$C_UNKNOWNS"
        printf 'MISSINGS=%d\n' "$C_MISSINGS"
        printf 'KIT=%s\n' "$KIT"
    } > "$STATE"
    return 0
}

finding_category() {
    local key=$1
    case "$key" in
        command:*|*.needed:*|*.abi:*|*.interpreter) printf 'dep' ;;
        CONFIG_*|active-lsm|btf|btf-*|lsm-hooks|kernel-config|kernel-config-format|tracepoints|syscalls:*|sched:*)
            printf 'kernel' ;;
        storage:*|mount:*) printf 'mount' ;;
        cgroup*|security-policy|runtime-boundary|jit|jit-runtime) printf 'optional' ;;
        architecture|systemd) printf 'host' ;;
        delivery-inputs:*|runtime-rpm*) printf 'material' ;;
        capabilities|privilege) printf 'privilege' ;;
        *) printf 'other' ;;
    esac
}

report_counts() {
    log_line "汇总：已确认阻断=$C_BLOCKS，随包可补齐=$C_MISSINGS，关键待确认=$C_UNKNOWNS，通过=$C_PASS"
}

# 人工处理项的三段式指引：现状 / 处理 / 生效 / 复核
manual_guidance() {
    local cat=$1 key=$2
    case "$cat" in
        kernel)
            case "$key" in
                active-lsm)
                    cat <<'EOF'
  处理：这是启动期参数，没有运行时开关。需在启动命令行追加 bpf，例如：
        sudo grubby --update-kernel=ALL --args="lsm=lockdown,yama,integrity,selinux,bpf"
        lsm= 的取值必须与 /sys/kernel/security/lsm 现有列表一致，只在末尾追加 bpf；
        改动前先备份 /boot/loader/entries 下的引导条目。
  生效：必须重启。请在部署方确认的维护窗口内执行，本脚本不做修改。
  复核：cat /sys/kernel/security/lsm 应包含 bpf，然后重新执行本脚本 check。
  回退：grubby --info=ALL 核对原参数后，用 --remove-args 去掉本次追加的 lsm= 值并重启。
EOF
                    ;;
                btf)
                    cat <<'EOF'
  处理：运行内核缺少 /sys/kernel/btf/vmlinux（CONFIG_DEBUG_INFO_BTF 未启用或未随内核发布）。
        安装软件包不能补齐，需要更换为启用该选项的兼容内核。
  生效：更换内核后重启生效。
  复核：ls -l /sys/kernel/btf/vmlinux 存在且可读，然后重新执行本脚本 check。
EOF
                    ;;
                tracepoints)
                    cat <<'EOF'
  处理：tracefs 可能未挂载。若内核支持且部署方确认：
        sudo mount -t tracefs tracefs /sys/kernel/tracing
        持久化需在 /etc/fstab 增加条目，由操作人员决定是否落盘。
  生效：挂载后立即生效（本次启动周期），重新挂载前不会自动恢复。
  复核：cat /sys/kernel/tracing/available_events | grep -E 'sys_enter_execve|sched_process_exec'
EOF
                    ;;
                CONFIG_*)
                    cat <<'EOF'
  处理：这是内核编译期选项，安装软件包无法补齐。二选一：
        1) 换用已启用该选项的兼容内核（由准备机或供应商出包）；
        2) 核对当前内核是否仅为配置未随包发布：sudo cat /boot/config-$(uname -r)。
        本脚本不会修改或替换内核。
  生效：更换内核需重启，属维护窗口操作。
  复核：sudo zcat /proc/config.gz 2>/dev/null | grep <该选项>，或 cat /boot/config-$(uname -r)。
EOF
                    ;;
                *)
                    cat <<'EOF'
  处理：属内核能力缺失，安装软件包无法补齐。按 deployment-operation-report.txt 中该条的处理方法执行，
        通常需要更换兼容内核或在部署方确认的维护窗口内调整启动参数。
  生效：涉及内核或启动参数的改动都需要重启。
  复核：重启后重新执行本脚本 check。
EOF
                    ;;
            esac
            ;;
        mount)
            cat <<'EOF'
  处理：目标目录所在挂载为只读或 noexec。需要由操作人员调整 /etc/fstab 中的挂载选项，
        并在维护窗口内 remount；本脚本不做 remount，也不改动 fstab。
  生效：remount 后立即生效；fstab 改动在下次挂载时生效。
  复核：findmnt -T <对应目录>，确认选项中不含 ro/noexec。
EOF
            ;;
        host)
            cat <<'EOF'
  处理：主机形态与交付物不匹配（架构不符，或 PID 1 不是 systemd）。
        容器内无法补齐宿主机的 BPF LSM，请改用符合条件的宿主机，或改用对应架构的交付物。
  生效：更换主机后重新执行本脚本 check。
  复核：uname -m; ps -p 1 -o comm=
EOF
            ;;
        material)
            cat <<'EOF'
  处理：随包离线仓库中没有可用的候选包，或交付物本身缺失。
        需要在准备机下载对应包及其完整依赖闭包，重建离线仓库与校验清单后重新交付；
        本机无法自行补齐。
  生效：重新交付并解包后执行本脚本 fix。
  复核：重新执行本脚本 check，确认该项转为 PASS 或消失。
EOF
            ;;
        privilege|other)
            cat <<'EOF'
  处理：证据不足，无法静态判定。按 deployment-operation-report.txt 中该条列出的
        "只读核对"命令补采证据；缺少诊断工具时先补工具再复查。
  生效：补采证据后重新检查即可，不涉及配置变更。
  复核：重新执行本脚本 check。
EOF
            ;;
        optional)
            case "$key" in
                jit|jit-runtime)
                    cat <<'EOF'
  处理：BPF JIT 未确认开启，只影响性能，不构成部署阻断。如需开启：
        临时：sudo sysctl -w net.core.bpf_jit_enable=1
        持久：写入 /etc/sysctl.d/ 下的配置文件后执行 sudo sysctl --system
        本脚本不修改 sysctl，是否开启由操作人员决定。
  生效：临时值立即生效；持久化配置在 sysctl --system 或重启后生效。
  复核：cat /proc/sys/net/core/bpf_jit_enable
EOF
                    ;;
                *)
                    cat <<'EOF'
  处理：属可选或性能相关项，不构成部署阻断，可保持现状。
  生效：无需变更。
  复核：如需优化，按 deployment-operation-report.txt 中的只读命令核对。
EOF
                    ;;
            esac
            ;;
        *)
            cat <<'EOF'
  处理：按 deployment-operation-report.txt 中该条的处理方法执行。
  生效：按该方法说明的生效条件执行。
  复核：处理后重新执行本脚本 check。
EOF
            ;;
    esac
}

print_findings() {
    local i cat
    if ((C_BLOCKS > 0)); then
        log_line ""
        log_line "== 阻断性缺失（需处理后才可继续）=="
        for i in "${!F_STATUS[@]}"; do
            [[ "${F_STATUS[$i]}" == BLOCK ]] || continue
            cat=$(finding_category "${F_KEY[$i]}")
            log_line "  [BLOCK] ${F_KEY[$i]}：${F_MSG[$i]}"
            log_line "          类别：$cat"
        done
    fi
    if ((C_MISSINGS > 0)); then
        log_line ""
        log_line "== 随包可补齐（fix 子命令会自动安装）=="
        for i in "${!F_STATUS[@]}"; do
            [[ "${F_STATUS[$i]}" == MISSING ]] || continue
            log_line "  [MISSING] ${F_KEY[$i]}"
            log_line "            候选包：${F_CAND[$i]:-未提供}"
        done
    fi
    if ((C_UNKNOWNS > 0)); then
        log_line ""
        log_line "== 关键待确认（证据不足，不能当作通过）=="
        for i in "${!F_STATUS[@]}"; do
            [[ "${F_STATUS[$i]}" == UNKNOWN && "${F_REQUIRED[$i]}" == true ]] || continue
            log_line "  [UNKNOWN] ${F_KEY[$i]}：${F_MSG[$i]}"
        done
    fi
}

write_summary() {
    local phase=$1 conclusion=$2 i cat idx=0
    {
        printf '# CensorGuard 目标主机部署摘要\n\n'
        printf -- '- 生成时间：%s\n' "$(date '+%Y-%m-%d %H:%M:%S')"
        printf -- '- 主机：%s（%s）\n' "$(uname -n 2>/dev/null || echo unknown)" "$(uname -m 2>/dev/null || echo unknown)"
        printf -- '- 运行内核：%s\n' "$(uname -r 2>/dev/null || echo unknown)"
        printf -- '- 交付包：%s\n' "$KIT"
        printf -- '- 本次执行：%s\n\n' "$phase"
        printf '## 环境结论\n\n%s\n\n' "$conclusion"
        printf '计数：阻断=%d，随包可补齐=%d，关键待确认=%d，通过=%d\n\n' \
            "$C_BLOCKS" "$C_MISSINGS" "$C_UNKNOWNS" "$C_PASS"

        printf '## 已自动补齐的依赖\n\n'
        if ((${#INSTALLED_PKGS[@]} == 0)); then
            printf '本次没有执行依赖安装。\n\n'
        else
            printf '| 包名 | 来源 RPM |\n| --- | --- |\n'
            local entry
            for entry in "${INSTALLED_PKGS[@]}"; do
                printf '| %s | %s |\n' "${entry%%|*}" "${entry#*|}"
            done
            printf '\n'
        fi

        printf '## 需要人工处理的事项\n\n'
        for i in "${!F_STATUS[@]}"; do
            case "${F_STATUS[$i]}" in BLOCK|UNKNOWN|WARN) ;; *) continue ;; esac
            if [[ "${F_STATUS[$i]}" == UNKNOWN && "${F_REQUIRED[$i]}" != true ]]; then continue; fi
            idx=$((idx + 1))
            cat=$(finding_category "${F_KEY[$i]}")
            printf '### %d. [%s] %s\n\n' "$idx" "${F_STATUS[$i]}" "${F_KEY[$i]}"
            printf '现状：%s\n\n' "${F_MSG[$i]}"
            printf '```text\n'
            manual_guidance "$cat" "${F_KEY[$i]}" | sed 's/^  //'
            printf '```\n\n'
        done
        if ((idx == 0)); then
            printf '无。\n\n'
        fi

        printf '## 服务状态\n\n'
        if [[ -f "$OUT/service-status.txt" ]]; then
            cat "$OUT/service-status.txt"
            printf '\n'
        else
            printf '本次未执行启动阶段。\n\n'
        fi
        printf '开机启动：未设置。本脚本只执行 systemctl start，不执行 systemctl enable。\n\n'
        printf '## 报告位置\n\n'
        printf -- '- 部署环境报告：%s/deployment-environment-report.txt\n' "$CHECK_DIR"
        printf -- '- 缺失及处理报告：%s/deployment-operation-report.txt\n' "$CHECK_DIR"
        printf -- '- 结论清单：%s\n' "$FINDINGS"
        printf -- '- 执行日志：%s\n' "$LOG"
    } > "$SUMMARY" || log_warn "摘要写入失败：$SUMMARY"
    log_info "部署摘要：$SUMMARY"
}

conclusion_text() {
    if ((C_BLOCKS > 0)); then
        printf '存在阻断性缺失，需按摘要中的人工处理事项处理后重新检查，暂不能继续部署。'
    elif ((C_UNKNOWNS > 0)); then
        printf '存在关键待确认事项，需补齐证据后重新检查；不能把信息不足当作兼容性通过。'
    elif ((C_MISSINGS > 0)); then
        printf '存在随包可补齐的依赖缺失，执行 fix 子命令安装后重新检查。'
    else
        printf '静态检查未发现阻断性缺失，可继续后续部署与运行验收。'
    fi
}

# ---------------------------------------------------------------- check

cmd_check() {
    run_check false || die 3 "环境检查未完成"
    report_counts
    print_findings
    write_summary "check" "$(conclusion_text)"
    if ((C_BLOCKS > 0)); then
        log_warn "存在阻断性缺失，详见 $SUMMARY"
        return 2
    fi
    if ((C_UNKNOWNS > 0)); then
        log_warn "存在关键待确认项，详见 $SUMMARY"
        return 3
    fi
    log_ok "环境检查通过"
    return 0
}

# ---------------------------------------------------------------- fix

collect_missing_candidates() {
    local i
    CAND_LIST=()
    for i in "${!F_STATUS[@]}"; do
        [[ "${F_STATUS[$i]}" == MISSING ]] || continue
        [[ -n "${F_CAND[$i]}" ]] || {
            log_warn "缺失项 ${F_KEY[$i]} 未提供候选包路径，跳过自动安装"
            continue
        }
        CAND_LIST+=("${F_CAND[$i]}")
    done
    ((${#CAND_LIST[@]} == 0)) && return 1
    return 0
}

cmd_fix() {
    local reuse=${1:-}
    if [[ "$reuse" == reuse && -s "$FINDINGS" ]]; then
        log_info "复用本轮检查结论，跳过重复扫描"
    else
        log_step "先确认当前缺失项"
        run_check false || die 3 "环境检查未完成"
    fi
    report_counts

    if ! collect_missing_candidates; then
        log_ok "没有可自动补齐的依赖缺失"
        write_summary "fix" "$(conclusion_text)"
        ((C_BLOCKS > 0)) && return 2
        ((C_UNKNOWNS > 0)) && return 3
        return 0
    fi

    local -a uniq=() pkgs=() verified=()
    local cand
    while IFS= read -r cand; do
        [[ -n "$cand" ]] && uniq+=("$cand")
    done < <(printf '%s\n' "${CAND_LIST[@]}" | sort -u)

    log_step "核验候选 RPM 签名（只验不装）"
    for cand in "${uniq[@]}"; do
        [[ -f "$cand" ]] || die 3 "候选 RPM 不存在：$cand"
        if [[ "$DRY_RUN" == true ]]; then
            printf '  [dry-run] rpm -K %s\n' "$cand"
            verified+=("$cand")
            continue
        fi
        if ! rpm -K "$cand" >> "$LOG" 2>&1; then
            log_err "签名核验失败：$cand"
            die 3 "候选 RPM 签名无效，停止安装"
        fi
        log_ok "$(basename -- "$cand") 签名有效"
        verified+=("$cand")
    done

    local name
    for cand in "${verified[@]}"; do
        name=$(rpm -qp --qf '%{NAME}' -- "$cand" 2>/dev/null || true)
        if [[ -z "$name" ]]; then
            if [[ "$DRY_RUN" == true ]]; then
                name="<$(basename -- "$cand" .rpm)>"
            else
                die 3 "无法读取包名：$cand"
            fi
        fi
        pkgs+=("$name")
    done
    local -a pkg_uniq=()
    while IFS= read -r name; do
        [[ -n "$name" ]] && pkg_uniq+=("$name")
    done < <(printf '%s\n' "${pkgs[@]}" | sort -u)

    log_step "导入随包 RPM 验签公钥"
    run rpm --import "$KIT/keys/RPM-GPG-KEY-openEuler" >> "$LOG" 2>&1 \
        || log_warn "公钥导入返回非零（可能已导入过）"

    local repo="cg-runtime,file://$KIT/rpms/runtime"
    local -a dnf_base=(dnf "--disablerepo=*" "--repofrompath=$repo" --enablerepo=cg-runtime
        --setopt=cg-runtime.gpgcheck=1 --setopt=cg-runtime.repo_gpgcheck=0
        --setopt=install_weak_deps=False)

    local preview="$OUT/transaction-preview.txt"
    log_step "预览安装事务（只解析依赖，不执行安装）"
    if [[ "$DRY_RUN" == true ]]; then
        printf '  [dry-run] %s install --assumeno %s\n' "${dnf_base[*]}" "${pkg_uniq[*]}"
    else
        "${dnf_base[@]}" --assumeno install "${pkg_uniq[@]}" > "$preview" 2>&1
        sed -n '1,120p' "$preview"
        local risky
        risky=$(grep -E '^[[:space:]]*(Installing|Upgrading|Updating|Downgrading|Reinstalling|Obsoleting)[[:space:]]*:[[:space:]]*(glibc|glibc-common|systemd|systemd-libs|systemd-udev|kernel|kernel-core|kernel-modules|openssl-libs)' "$preview" || true)
        if [[ -n "$risky" ]]; then
            log_warn "本次事务包含基础系统组件变更："
            while IFS= read -r line; do log_line "  $line"; done <<< "$risky"
            log_warn "这类变更影响整机，需部署方确认并留存回退方案。"
            confirm_typed "确认执行包含基础系统组件变更的安装事务？" || die 3 "已取消安装"
        else
            confirm "确认执行上述安装事务？" || die 3 "已取消安装"
        fi
    fi

    log_step "安装随包运行依赖：${pkg_uniq[*]}"
    if [[ "$DRY_RUN" != true ]]; then
        "${dnf_base[@]}" install "${pkg_uniq[@]}" 2>&1 | tee -a "$LOG"
        local rc=${PIPESTATUS[0]}
        if ((rc != 0)); then
            log_err "dnf 安装失败，返回 $rc"
            die 4 "依赖安装失败"
        fi
    fi
    local p
    for p in "${pkg_uniq[@]}"; do
        INSTALLED_PKGS+=("$p|$(rpm -q --qf '%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}' "$p" 2>/dev/null || echo 已安装)")
    done
    log_ok "依赖安装完成"

    log_step "安装后复查（保留安装前报告以便对照）"
    run_check true || die 3 "复查未完成"
    report_counts
    print_findings
    write_summary "fix + 复查" "$(conclusion_text)"
    if ((C_BLOCKS > 0)); then
        log_warn "复查仍有阻断性缺失，详见 $SUMMARY"
        return 2
    fi
    if ((C_UNKNOWNS > 0)); then
        log_warn "复查仍有待确认项，详见 $SUMMARY"
        return 3
    fi
    log_ok "依赖已补齐，复查通过"
    return 0
}

# ---------------------------------------------------------------- install

mark_service_status() {
    local unit_state enabled_state
    unit_state=$(systemctl is-active "$UNIT" 2>/dev/null || true)
    enabled_state=$(systemctl is-enabled "$UNIT" 2>/dev/null || true)
    {
        printf -- '- 单元：%s\n' "$UNIT"
        printf -- '- 运行状态：%s\n' "${unit_state:-unknown}"
        printf -- '- 开机启动状态：%s\n' "${enabled_state:-unknown}"
        printf -- '- 套接字：\n'
        local s
        for s in "${SOCKETS[@]}"; do
            if [[ -S "$s" ]]; then printf '  - %s 存在\n' "$s"
            else printf '  - %s 未找到\n' "$s"; fi
        done
        if [[ -f "$OUT/doctor.txt" ]]; then
            printf -- '- doctor 输出：\n```\n'
            sed -n '1,40p' "$OUT/doctor.txt"
            printf '```\n'
        fi
    } > "$OUT/service-status.txt"
    if [[ "$enabled_state" == enabled ]]; then
        log_warn "检测到 $UNIT 已设置为开机启动（可能是历史遗留）；本脚本不会改动它。"
        log_warn "如需关闭：systemctl disable $UNIT"
    fi
}

cmd_install() {
    log_step "安装前条件核对"

    if [[ ! -f "$STATE" ]]; then
        log_info "尚无本轮检查结论，先执行一次环境检查"
        run_check false || die 3 "环境检查未完成"
    fi
    # shellcheck disable=SC1090
    source "$STATE" 2>/dev/null || true
    local blocks=${BLOCKS:-0} unknowns=${UNKNOWNS:-0} missings=${MISSINGS:-0}
    log_info "最近一次检查：${LAST_CHECK_TS:-未知}（阻断=$blocks 待确认=$unknowns 可补齐=$missings）"

    if ((blocks > 0 || unknowns > 0)); then
        if [[ "$FORCE" != true ]]; then
            log_err "最近一次检查存在阻断或关键待确认项，先处理后再安装："
            log_err "  详细指引：$SUMMARY"
            log_err "  如确需跳过：--force（风险自担）"
            return 2
        fi
        log_warn "已用 --force 跳过环境结论，安装后仍须完成运行验收"
    fi
    if ((missings > 0)); then
        log_warn "仍有 $missings 项随包可补齐的依赖未安装，运行期可能因缺库失败"
        confirm "仍然继续安装文件与初始配置？" || die 3 "已取消安装"
    fi

    if [[ -e "$DAEMON_BIN" ]]; then
        if [[ "$FORCE" != true ]]; then
            log_err "检测到已安装：$DAEMON_BIN 已存在"
            log_err "本脚本只做首次安装。升级场景请按变更流程单独处理；确需覆盖请加 --force。"
            return 3
        fi
        log_warn "已存在安装（--force）：将覆盖同名文件，并先备份配置"
    fi
    if [[ "$(systemctl is-active "$UNIT" 2>/dev/null || true)" == active ]]; then
        log_err "$UNIT 正在运行，脚本不会在运行期替换二进制。"
        log_err "请先 sudo systemctl stop $UNIT 后重新执行。"
        return 3
    fi

    if [[ -d /etc/censorguard ]]; then
        local backup="$OUT/backup-etc-censorguard-$RUN_TS.tar.gz"
        log_step "备份现有配置目录到 $backup"
        run tar -czf "$backup" -C /etc censorguard || log_warn "配置备份失败，请人工确认后再继续"
    fi

    log_step "解包安装文件树到 /"
    confirm "确认解包安装文件树到 / 并继续？" || die 3 "已取消安装"
    run tar -C / -xzf "$KIT/release/CensorGuard-standalone-aarch64.tar.gz" \
        || die 4 "解包安装文件树失败"
    run systemd-sysusers /usr/lib/sysusers.d/censorguard.conf \
        || die 4 "创建系统用户失败"
    run systemd-tmpfiles --create /usr/lib/tmpfiles.d/censorguard.conf \
        || die 4 "创建运行目录失败"
    run install -d -m 0750 -o root -g censorguard /etc/censorguard /var/lib/censorguard \
        || die 4 "创建配置与状态目录失败"

    if [[ -e "$CONFIG_FILE" ]]; then
        log_info "保留已有配置：$CONFIG_FILE（不覆盖）"
    else
        log_step "从 base.yaml.example 生成初始配置"
        run install -m 0640 -o root -g censorguard \
            /etc/censorguard/base.yaml.example "$CONFIG_FILE" \
            || die 4 "生成初始配置失败"
    fi
    run systemctl daemon-reload || log_warn "daemon-reload 返回非零，请复核"

    log_step "配置语法检查"
    if [[ "$DRY_RUN" == true ]]; then
        printf '  [dry-run] %s launch --config %s --check-config\n' "$DAEMON_BIN" "$CONFIG_FILE"
    else
        if ! "$DAEMON_BIN" launch --config "$CONFIG_FILE" --check-config 2>&1 | tee -a "$LOG"; then
            die 4 "初始配置未通过检查，已停止，未启动服务"
        fi
    fi

    log_step "启动服务（仅 start，不 enable）"
    run systemctl start "$UNIT" || die 4 "服务启动失败，查看：journalctl -u $UNIT -n 50"
    if [[ "$DRY_RUN" != true ]]; then
        local i ready=false
        for ((i = 0; i < 20; i++)); do
            ready=true
            local s
            for s in "${SOCKETS[@]}"; do
                [[ -S "$s" ]] || ready=false
            done
            [[ "$ready" == true ]] && break
            sleep 1
        done
        [[ "$ready" == true ]] || log_warn "等待 20 秒后套接字仍不完整，服务可能未就绪"
        systemctl status "$UNIT" --no-pager 2>&1 | tee -a "$LOG" | sed -n '1,12p'
        if [[ -x "$CTL_BIN" ]]; then
            "$CTL_BIN" doctor > "$OUT/doctor.txt" 2>&1
            log_info "doctor 返回 $?，详见 $OUT/doctor.txt"
            "$CTL_BIN" status --json > "$OUT/status.json" 2>&1 || log_warn "status --json 返回非零"
        fi
    fi
    mark_service_status
    write_summary "install" "安装与启动已完成，需按指南完成运行验收与纳管。"

    log_ok "安装完成。服务已启动，未设置开机启动。"
    log_info "下一步：确认业务运行正常后，按指南第 4 节分阶段加载配置；attach 需使用实际业务 PID。"
    return 0
}

# ---------------------------------------------------------------- all

cmd_all() {
    local rc=0
    cmd_check || rc=$?
    if ((rc == 2)); then
        log_err "存在阻断性缺失，停止流程。处理后可重复执行本命令。"
        return 2
    fi
    if ((rc == 3)); then
        log_err "存在关键待确认项，停止流程。补齐证据后可重复执行本命令。"
        return 3
    fi
    if ((C_MISSINGS > 0)); then
        cmd_fix reuse || rc=$?
        ((rc == 0)) || return "$rc"
    else
        log_ok "无需安装额外依赖"
    fi
    if [[ "$ASSUME_YES" != true ]]; then
        confirm "环境检查已通过，是否继续安装文件并启动服务？" || return 0
    fi
    cmd_install
}

# ---------------------------------------------------------------- main

main() {
    if (($# == 0)); then
        usage
        return 1
    fi
    case "$1" in
        -h|--help) usage; return 0 ;;
        check|fix|install|all) SUBCOMMAND=$1; shift ;;
        *) printf '未知子命令：%s\n\n' "$1" >&2; usage >&2; return 1 ;;
    esac
    while (($#)); do
        case "$1" in
            --kit) KIT=${2:-}; [[ -n "$KIT" ]] || { log_err "--kit 缺少目录"; return 1; }; shift 2 ;;
            --out) OUT=${2:-}; [[ -n "$OUT" ]] || { log_err "--out 缺少目录"; return 1; }; shift 2 ;;
            --log) LOG=${2:-}; [[ -n "$LOG" ]] || { log_err "--log 缺少文件"; return 1; }; shift 2 ;;
            --yes) ASSUME_YES=true; shift ;;
            --dry-run) DRY_RUN=true; shift ;;
            --skip-verify) SKIP_VERIFY=true; shift ;;
            --force) FORCE=true; shift ;;
            -h|--help) usage; return 0 ;;
            *) printf '未知选项：%s\n\n' "$1" >&2; usage >&2; return 1 ;;
        esac
    done
    [[ "$OUT" == /* ]] || OUT="$PWD/$OUT"
    if [[ -n "$LOG" && "$LOG" != /* ]]; then LOG="$PWD/$LOG"; fi
    preflight
    case "$SUBCOMMAND" in
        check) cmd_check ;;
        fix) cmd_fix ;;
        install) cmd_install ;;
        all) cmd_all ;;
    esac
}

if [[ "${BASH_SOURCE[0]:-$0}" == "$0" ]]; then main "$@"; exit "$?"; fi
