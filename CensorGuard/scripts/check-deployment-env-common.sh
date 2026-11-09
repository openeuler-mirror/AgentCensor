#!/usr/bin/env bash
# Collection failures must not prevent unrelated checks from completing.
set +e
set -u
set -o pipefail
export LC_ALL=C LANG=C SYSTEMD_PAGER=cat SYSTEMD_COLORS=0
umask 077

usage() {
    printf '%s\n' \
        '用法：bash check-deployment-env.sh --output 目录 [--arch aarch64|x86_64|any] [--kit 交付包根目录]' \
        '       [--daemon 文件] [--ctl 文件] [--bpf-object 文件] [--config 文件]' \
        '       [--rpm-dir 运行依赖RPM目录] [--timeout 秒] [--keep-previous] [--findings 文件]' \
        '完整采集部署环境，生成部署环境报告与缺失及处理报告。' \
        '--output 必填：由操作人员指定本次报告的目录，脚本不设默认值。' \
        '--findings 输出机器可读的结论清单（每行五列，TAB 分隔：状态、键、是否必需、描述、候选包），' \
        '  供部署脚本分派使用；省略时写到 <output>/findings.tsv。人工阅读仍以两份报告为准。' \
        '  目录已存在时替换其中的上一轮报告；--keep-previous 先将上一轮报告移到' \
        '  <目录>/previous/<时间戳>/ 保留。' \
        '未显式指定的交付物按交付包相对路径自动识别（脚本位于 <交付包>/tools/）：' \
        '  release/rootfs/usr/sbin/censorguardd、release/rootfs/usr/bin/censorguardctl、' \
        '  release/rootfs/usr/lib/censorguard/enforce.bpf.o、release/rootfs/etc/censorguard/base.yaml.example、' \
        '  rpms/runtime；源码树中则识别 target/release/、bpf/、config/ 下的构建产物。' \
        '自动识别失败时报告会列出已查找路径，并要求用对应选项显式指定。' \
        '退出码：0 报告生成完成（含发现阻断项）；2 参数错误；3 报告目录、写入或临时区清理失败。'
}

append_text() {
    local source=$1 destination=$2 line
    if command -v cat >/dev/null 2>&1; then
        cat -- "$source" >> "$destination" || WRITE_ERROR=1
    else
        while IFS= read -r line || [[ -n "$line" ]]; do
            printf '%s\n' "$line" >> "$destination" || WRITE_ERROR=1
        done < "$source" || WRITE_ERROR=1
    fi
}

# 交付包根目录：脚本位于 <交付包>/tools/，源码树中位于 <仓库>/scripts/。
kit_root() {
    local source=${BASH_SOURCE[0]:-$0} directory
    directory=${source%/*}
    [[ "$directory" != "$source" ]] || directory=.
    directory=$(cd -- "$directory" 2>/dev/null && pwd -P) || return 1
    printf '%s' "${directory%/*}"
}

# 依次检查候选路径，第一个存在的文件/目录写入 AUTO_PATH。
resolve_first() {
    local kind=$1 candidate
    shift
    AUTO_PATH=''
    for candidate in "$@"; do
        if [[ "$kind" == dir ]]; then
            [[ -d "$candidate" ]] || continue
        else
            [[ -f "$candidate" ]] || continue
        fi
        AUTO_PATH=$candidate
        return 0
    done
    return 1
}

# 交付物默认来自同一个交付包，先按相对路径识别；显式参数始终优先。
discover_inputs() {
    local base=${CG_KIT_ROOT:-} item kind current searched
    local -a candidates=()
    INPUT_SOURCE=() INPUT_SEARCHED=()
    for item in daemon ctl bpf-object config rpm-dir; do
        kind=file
        case "$item" in
            daemon)
                current=$DAEMON
                candidates=("$base/release/rootfs/usr/sbin/censorguardd" "$base/target/release/censorguardd" "$base/usr/sbin/censorguardd") ;;
            ctl)
                current=$CTL
                candidates=("$base/release/rootfs/usr/bin/censorguardctl" "$base/target/release/censorguardctl" "$base/usr/bin/censorguardctl") ;;
            bpf-object)
                current=$BPF_OBJECT
                candidates=("$base/release/rootfs/usr/lib/censorguard/enforce.bpf.o" "$base/bpf/enforce.bpf.o" "$base/usr/lib/censorguard/enforce.bpf.o") ;;
            config)
                current=$CONFIG
                candidates=("$base/release/rootfs/etc/censorguard/base.yaml.example" "$base/release/rootfs/etc/censorguard/base.yaml" "$base/config/deployment.yaml") ;;
            rpm-dir)
                current=$RPM_DIR kind=dir
                candidates=("$base/rpms/runtime") ;;
        esac
        if [[ -n "$current" ]]; then
            INPUT_SOURCE[$item]=explicit
            continue
        fi
        if resolve_first "$kind" "${candidates[@]}"; then
            INPUT_SOURCE[$item]=auto
            case "$item" in
                daemon) DAEMON=$AUTO_PATH ;; ctl) CTL=$AUTO_PATH ;; bpf-object) BPF_OBJECT=$AUTO_PATH ;;
                config) CONFIG=$AUTO_PATH ;; rpm-dir) RPM_DIR=$AUTO_PATH ;;
            esac
            continue
        fi
        printf -v searched '%s  ' "${candidates[@]}"
        INPUT_SOURCE[$item]=missing
        INPUT_SEARCHED[$item]=${searched%  }
    done
}

input_path() {
    case "$1" in
        daemon) printf '%s' "${DAEMON:-}" ;;
        ctl) printf '%s' "${CTL:-}" ;;
        bpf-object) printf '%s' "${BPF_OBJECT:-}" ;;
        config) printf '%s' "${CONFIG:-}" ;;
        rpm-dir) printf '%s' "${RPM_DIR:-}" ;;
    esac
}

report_inputs() {
    local item value
    fact kit_root "${CG_KIT_ROOT:-unknown}"
    for item in daemon ctl bpf-object config rpm-dir; do
        value=$(input_path "$item")
        fact "input:$item" "${value:-未找到}"
        fact "input-source:$item" "${INPUT_SOURCE[$item]:-unknown}"
        [[ -n "$value" ]] || fact "input-search:$item" "${INPUT_SEARCHED[$item]:-无}"
    done
}

# 目录为空，或是本脚本上一轮生成的报告目录（含两份报告或临时区）时可复用。
reusable_report_dir() {
    local dir=$1 entry content
    for entry in deployment-environment-report.txt deployment-operation-report.txt findings.tsv raw; do
        if [[ -e "$dir/$entry" || -L "$dir/$entry" ]]; then return 0; fi
    done
    content=$(ls -A -- "$dir" 2>/dev/null) || return 1
    [[ -z "$content" ]]
}

clear_report_dir() {
    local dir=$1 entry stamp destination='' replaced=false
    for entry in deployment-environment-report.txt deployment-operation-report.txt findings.tsv; do
        [[ -e "$dir/$entry" || -L "$dir/$entry" ]] || continue
        replaced=true
        if [[ "$KEEP_PREVIOUS" == true ]]; then
            if [[ -z "$destination" ]]; then
                printf -v stamp '%(%Y%m%dT%H%M%S)T' -1
                destination="$dir/previous/$stamp"
                mkdir -p -m 0700 -- "$destination" || return 1
            fi
            mv -f -- "$dir/$entry" "$destination/$entry" || return 1
        else
            rm -f -- "$dir/$entry" || return 1
        fi
    done
    [[ "$replaced" == true ]] && REPLACED_PREVIOUS=true
    [[ ! -e "$dir/raw" && ! -L "$dir/raw" ]] && return 0
    if [[ -d "$dir/raw" && ! -L "$dir/raw" ]]; then rm -r -- "$dir/raw" || return 1
    else rm -f -- "$dir/raw" || return 1
    fi
}

init_report() {
    local requested=${1:-} stamp
    if [[ -z "$requested" ]]; then
        printf '%s\n' '未指定报告目录：本脚本不设默认输出路径，必须用 --output 指定。' >&2
        return 3
    fi
    [[ "$requested" == /* ]] || requested="$PWD/$requested"
    if [[ -L "$requested" ]]; then
        printf '报告路径是符号链接，拒绝写入：%s\n' "$requested" >&2
        return 3
    fi
    if [[ -e "$requested" && ! -d "$requested" ]]; then
        printf '报告路径已存在且不是目录：%s\n' "$requested" >&2
        return 3
    fi
    if [[ -d "$requested" ]]; then
        if [[ ! -w "$requested" || ! -x "$requested" ]]; then
            printf '报告目录不可写（可能由其他用户生成，例如上一轮用 root 执行）：%s\n' "$requested" >&2
            printf '请用同一用户或 root 重新执行，或改用 --output 指定其他目录。\n' >&2
            return 3
        fi
        if ! reusable_report_dir "$requested"; then
            printf '报告目录非空且不是本脚本上一轮生成的目录，拒绝清理：%s\n' "$requested" >&2
            printf '请改用空目录或新的路径；脚本只替换自己生成的报告文件。\n' >&2
            return 3
        fi
        clear_report_dir "$requested" || {
            printf '无法替换上一轮报告文件：%s\n' "$requested" >&2
            return 3
        }
        chmod 0700 -- "$requested" 2>/dev/null || :
    else
        mkdir -m 0700 -- "$requested" || return 3
    fi
    OUT=$requested
    mkdir -- "$OUT/raw" || return 3
    : > "$OUT/deployment-environment-report.txt" || return 3
    : > "$OUT/raw/actions.txt" || return 3
    FINDINGS_FILE=${FINDINGS_FILE:-$OUT/findings.tsv}
    printf '# 状态\t键\t是否必需\t描述\t候选包\n' > "$FINDINGS_FILE" || return 3
    printf -v stamp '%(%Y-%m-%dT%H:%M:%S%z)T' -1
    printf '%s\n' 'CensorGuard 部署环境报告' \
        '范围：系统现状、运行内核能力、交付物和随包 RPM 元数据。' \
        '每项采集记录命令、返回码、耗时、输出和错误；无法采集的项目保留原因。' \
        "生成时间：$stamp" "报告目录：$OUT" "交付包根目录：${CG_KIT_ROOT:-unknown}" \
        > "$OUT/deployment-environment-report.txt" || return 3
    COUNT=0 BLOCKS=0 UNKNOWNS=0 MISSING=0 WRITE_ERROR=0
}

fact() {
    printf '[事实] %s: %s\n' "$1" "$2" >> "$OUT/deployment-environment-report.txt" || WRITE_ERROR=1
}

finding() {
    local status=$1 key=$2 message=$3 required=$4 item
    shift 4
    [[ "$status" != BLOCK ]] || BLOCKS=$((BLOCKS + 1))
    [[ "$status" != UNKNOWN || "$required" != true ]] || UNKNOWNS=$((UNKNOWNS + 1))
    [[ "$status" != MISSING ]] || MISSING=$((MISSING + 1))
    printf '[%s] %s: %s\n' "$status" "$key" "$message"
    printf '[%s] %s: %s\n' "$status" "$key" "$message" >> "$OUT/deployment-environment-report.txt" || WRITE_ERROR=1
    # 机器可读结论：候选包放在最后一列，空值不会吞掉前面的列。
    if [[ -n "${FINDINGS_FILE:-}" ]]; then
        printf '%s\t%s\t%s\t%s\t%s\n' "$status" "$key" "$required" \
            "${message//$'\t'/ }" "${FINDING_CANDIDATE:-}" >> "$FINDINGS_FILE" || WRITE_ERROR=1
    fi
    FINDING_CANDIDATE=''
    if [[ "$status" == BLOCK || "$status" == UNKNOWN || "$status" == MISSING || "$status" == WARN || $# -gt 0 ]]; then
        {
            printf '\n[%s] %s\n现状：%s\n' "$status" "$key" "$message"
            case "$status" in
                BLOCK) printf '性质：阻断性缺失，当前环境或交付材料需要处理。\n' ;;
                MISSING) printf '性质：随包有候选 RPM 可补齐，安装事务及兼容性待现场复核。\n' ;;
                UNKNOWN) printf '性质：信息不足，不能据此确认缺失或兼容。\n' ;;
                *) printf '性质：补充说明或需人工复核事项。\n' ;;
            esac
            if (($# == 0)); then
                printf '处理方法：核对上述检查项的原始命令和错误，补充证据后复查。\n'
            else
                for item in "$@"; do printf '处理方法：%s\n' "$item"; done
            fi
        } >> "$OUT/raw/actions.txt" || WRITE_ERROR=1
    fi
}

collect() {
    local key=$1 required=$2 start=$SECONDS
    shift 2
    COUNT=$((COUNT + 1))
    printf -v CF '%s/raw/%04d.txt' "$OUT" "$COUNT"
    CE="$CF.stderr"
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'command unavailable: %s\n' "$1" > "$CE"
        : > "$CF"; RC=127
    elif ! command -v timeout >/dev/null 2>&1; then
        printf '%s\n' 'timeout unavailable; skipped to avoid an unbounded external command' > "$CE"
        : > "$CF"; RC=125
    else
        # Limit report growth even when a diagnostic command emits excessive output.
        (ulimit -f 65536 || exit 125; exec timeout --signal=TERM --kill-after=2 "$TIMEOUT" "$@") > "$CF" 2> "$CE"
        RC=$?
    fi
    {
        printf '\n--- 采集 %04d: %s ---\n命令：' "$COUNT" "$key"
        printf '%q ' "$@"
        printf '\n返回码：%d；耗时：%d 秒\n' "$RC" "$((SECONDS - start))"
    } >> "$OUT/deployment-environment-report.txt" || WRITE_ERROR=1
    case "$key" in
        file:/sys/kernel/btf/vmlinux|file:/proc/config.gz)
            printf '输出：二进制内容留在检查临时区供解析；解析结果另列。\n' >> "$OUT/deployment-environment-report.txt" || WRITE_ERROR=1 ;;
        *) append_text "$CF" "$OUT/deployment-environment-report.txt" ;;
    esac
    printf '\n标准错误：\n' >> "$OUT/deployment-environment-report.txt" || WRITE_ERROR=1
    append_text "$CE" "$OUT/deployment-environment-report.txt"
    if ((RC != 0)); then
        if [[ "$required" == true ]]; then
            finding UNKNOWN "$key" "采集失败（exit=$RC），错误已记录，继续其他检查。" true
        else
            finding WARN "$key" "可选信息采集失败（exit=$RC），错误已记录，继续其他检查。" false
        fi
    fi
    return "$RC"
}

read_file() { collect "file:$1" false cat -- "$1"; }

kernel_remedy() {
    finding "$1" "$2" "$3" true \
        '无需重启的选择：换用已有兼容主机，或暂缓部署；安装开发包不能补齐当前内核能力。' \
        '只读核对：uname -r; cat /proc/cmdline; sudo grubby --info=ALL' \
        '仅另行批准维护窗口后，按部署指南准备兼容内核/完整 LSM 启动列表/回退方案；不要在当前业务窗口修改引导或重启。'
}

check_bundle() {
    local package line name='' package_arch='' count=0
    fact runtime_rpm_directory "$RPM_DIR"
    if [[ -z "$RPM_DIR" || ! -d "$RPM_DIR" || ! -r "$RPM_DIR" || ! -x "$RPM_DIR" ]]; then
        finding UNKNOWN runtime-rpms '未提供可读取的随包 runtime RPM 目录，无法判断缺失依赖是否已经准备。' true \
            "已按交付包相对路径查找：${INPUT_SEARCHED[rpm-dir]:-未尝试}" \
            '确认交付包完整解包，或用 --rpm-dir "$HOME/CensorGuard/rpms/runtime" 指定实际目录后复查；检查发生在目标主机。'
        return
    fi
    BUNDLE_COMPLETE=true
    for package in "$RPM_DIR"/*.rpm; do
        [[ -e "$package" ]] || continue
        count=$((count + 1))
        if ! collect "rpm-metadata:$package" true rpm -qp --qf '%{NAME}\n%{ARCH}\n[%{PROVIDENAME}\n]' -- "$package"; then
            BUNDLE_COMPLETE=false
            continue
        fi
        {
            IFS= read -r name || name=''
            IFS= read -r package_arch || package_arch=''
            if [[ -z "$name" || -z "$package_arch" ]]; then
                BUNDLE_COMPLETE=false
                finding UNKNOWN runtime-rpm-format "包元数据不完整：$package" true '核对 RPM 完整性和 rpm 查询输出。'
                continue
            fi
            if [[ "$package_arch" != "$MACHINE" && "$package_arch" != noarch ]]; then
                finding WARN runtime-rpm-arch "包 $package 架构为 $package_arch，不计入本机候选依赖。" false \
                    '准备与目标主机架构相符的 RPM 和传递依赖。'
                continue
            fi
            BUNDLE_PACKAGES[$name]+="$package"$'\n'
            BUNDLE_PACKAGE_NAMES[$package]=$name
            while IFS= read -r line; do
                [[ -z "$line" ]] || BUNDLE_PROVIDES[$line]+="$package"$'\n'
            done
        } < "$CF"
    done
    fact runtime_rpm_count "$count"
    if ((count == 0)); then
        finding WARN runtime-rpms-empty 'runtime 目录没有 RPM；若本机缺失依赖则交付包无法补齐。' false \
            '在准备机下载所需 RPM 及完整传递依赖，更新离线包和校验清单。'
    fi
    finding INFO runtime-rpm-scope '已核对随包 RPM 的名称、架构和 Provides；候选包不代表安装事务或 ABI 已验收。' false
}

first_candidate_path() {
    local candidates=$1
    candidates=${candidates%%$'\n'*}
    printf '%s' "$candidates"
}

shell_quote() {
    local value=$1
    value=${value//\'/\'\\\'\'}
    printf "'%s'" "$value"
}

install_commands() {
    local package=$1 candidate_path=${2:-} repo_arg package_arg candidate_arg
    repo_arg=$(shell_quote "cg-runtime,file://$RPM_DIR")
    printf -v package_arg '%q' "$package"
    printf -v candidate_arg '%q' "$candidate_path"
    INSTALL_VERIFY_COMMAND="sudo rpm -K $candidate_arg"
    INSTALL_PREVIEW_COMMAND="sudo dnf --disablerepo='*' --repofrompath=$repo_arg --enablerepo=cg-runtime --setopt=cg-runtime.gpgcheck=1 --setopt=cg-runtime.repo_gpgcheck=0 --setopt=install_weak_deps=False install --assumeno $package_arg"
    INSTALL_COMMAND="sudo dnf --disablerepo='*' --repofrompath=$repo_arg --enablerepo=cg-runtime --setopt=cg-runtime.gpgcheck=1 --setopt=cg-runtime.repo_gpgcheck=0 --setopt=install_weak_deps=False install $package_arg"
}

report_missing_dependency() {
    local key=$1 capability=$2 message=$3 verified=$4 candidate=${BUNDLE_PROVIDES[$2]:-} status=UNKNOWN
    local candidate_path candidate_package candidate_display
    if [[ -n "$candidate" ]]; then
        candidate_path=$(first_candidate_path "$candidate")
        candidate_display=${candidate//$'\n'/, }
        candidate_package=${BUNDLE_PACKAGE_NAMES[$candidate_path]:-}
        [[ -n "$candidate_package" ]] || candidate_package=$candidate_path
        install_commands "$candidate_package" "$candidate_path"
        FINDING_CANDIDATE=$candidate_path
        finding MISSING "$key" "$message；随包有候选提供者：$candidate_display" false \
            "核验候选 RPM：$INSTALL_VERIFY_COMMAND" \
            "先预览安装事务：$INSTALL_PREVIEW_COMMAND" \
            "确认预览结果后安装：$INSTALL_COMMAND" \
            '安装完成后重新运行环境检查（同一报告目录会替换上一轮报告），复核完整依赖闭包和符号版本兼容性。'
    else
        [[ "$verified" != true || "$BUNDLE_COMPLETE" != true ]] || status=BLOCK
        FINDING_CANDIDATE=''
        finding "$status" "$key" "$message；随包目录未找到 $capability 的兼容架构候选包。" true \
            "只读核对：ldconfig -p; rpm -q --whatprovides '$capability'" \
            "在准备机从批准仓库下载提供 '$capability' 的 RPM 及完整依赖闭包，重建离线仓库和校验清单后交付；或提供适配目标主机现有库的构建。" \
            '当前内核能力缺失应按对应内核检查项处理，运行库安装不能补齐内核能力。'
    fi
}
