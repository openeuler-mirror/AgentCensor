load_elf() {
    local path=$1 parsed kind value extra
    if [[ -n "${ELF_FILES[$path]+present}" ]]; then
        EP=${ELF_FILES[$path]}; [[ -n "$EP" ]]; return
    fi
    ELF_FILES[$path]=''
    if ! collect "elf:$path" true readelf -W -h -l -d -SW --dyn-syms --version-info -- "$path"; then return 1; fi
    local raw=$CF
    if ! collect "elf-parse:$path" true awk '
        /^[ \t]*Class:/ { print "C\t" $2 }
        /^[ \t]*Data:/ { sub(/^[ \t]*Data:[ \t]*/, ""); print "B\t" $0 }
        /^[ \t]*Machine:/ { sub(/^[ \t]*Machine:[ \t]*/, ""); print "M\t" $0 }
        /^[ \t]*Type:/ { print "T\t" $2 }
        /Requesting program interpreter:/ { s=$0; sub(/^.*Requesting program interpreter: */, "", s); sub(/\].*$/, "", s); print "I\t" s }
        /\(NEEDED\)/ { s=$0; sub(/^.*\[/, "", s); sub(/\].*$/, "", s); print "D\t" s }
        /\((RPATH|RUNPATH)\)/ { s=$0; sub(/^.*\[/, "", s); sub(/\].*$/, "", s); print "P\t" s }
        /^[ \t]*\[[ \t]*[0-9]+\][ \t]+/ { s=$0; sub(/^[ \t]*\[[ \t]*[0-9]+\][ \t]+/, "", s); split(s,a,/[ \t]+/); print "S\t" a[1] }
        $1 ~ /^[0-9]+:$/ && NF >= 8 && $2 ~ /^[0-9a-f]+$/ {
            n=$8; gsub(/@@/, "@", n)
            if ($7=="UND" && $5=="GLOBAL") print "R\t" n
            else if ($7!="UND" && ($5=="GLOBAL" || $5=="WEAK" || $5=="UNIQUE") && ($6=="DEFAULT" || $6=="PROTECTED")) {
                print "E\t" n
                if (index($8,"@@") || !index(n,"@")) {sub(/@.*/, "", n); print "E\t" n}
            }
        }
        /^Version definition section/ { section="definitions" }
        /^Version needs section/ { section="needs" }
        /^Version symbols section/ { section="" }
        section=="needs" && /File:/ { for(i=1;i<NF;i++) if($i=="File:") provider=$(i+1) }
        /Name:/ {
            for(i=1;i<NF;i++) if($i=="Name:") {
                if(section=="definitions") print "V\t" $(i+1)
                else if(section=="needs" && $0 !~ /Flags: WEAK/) print "N\t" provider "\t" $(i+1)
            }
        }
        ' "$raw"; then return 1; fi
    parsed=$CF
    ELF_FILES[$path]=$parsed
    ELF_RAW[$path]=$raw
    while IFS=$'\t' read -r kind value extra; do
        case "$kind" in
            C) ELF_CLASS[$path]=$value ;; B) ELF_ENDIAN[$path]=$value ;; M) ELF_MACHINE[$path]=$value ;;
            T) ELF_TYPE[$path]=$value ;;
        esac
    done < "$parsed"
    EP=$parsed
}

elf_arch_matches() {
    [[ "${ELF_CLASS[$1]:-}" == ELF64 && "${ELF_ENDIAN[$1]:-}" == *little* ]] || return 1
    case "$2" in
        aarch64) [[ "${ELF_MACHINE[$1]:-}" == AArch64 ]] ;;
        x86_64) [[ "${ELF_MACHINE[$1]:-}" == 'Advanced Micro Devices X86-64' ]] ;;
        bpf) [[ "${ELF_MACHINE[$1]:-}" == 'Linux BPF' || "${ELF_MACHINE[$1]:-}" == BPF ]] ;;
        *) return 1 ;;
    esac
}

resolve_library() {
    local soname=$1 base=$2 rpath=$3 folder candidate candidates='' rest
    RESOLVED='' RESOLUTION_UNKNOWN=false
    rest=$rpath
    while [[ -n "$rest" ]]; do
        folder=${rest%%:*}
        if [[ "$rest" == *:* ]]; then rest=${rest#*:}; else rest=''; fi
        folder=${folder//\$\{ORIGIN\}/$base}; folder=${folder//\$ORIGIN/$base}
        [[ "$folder" == /* && "$folder" != *'$'* ]] && candidates+="$folder/$soname"$'\n'
    done
    candidates+=${LIBRARIES[$soname]:-}
    for folder in /lib64 /usr/lib64 /lib /usr/lib /lib/aarch64-linux-gnu /usr/lib/aarch64-linux-gnu /lib/x86_64-linux-gnu /usr/lib/x86_64-linux-gnu; do
        candidates+="$folder/$soname"$'\n'
    done
    while IFS= read -r candidate; do
        if [[ -d "${candidate%/*}" && ! -x "${candidate%/*}" ]]; then RESOLUTION_UNKNOWN=true; fi
        [[ -n "$candidate" && -f "$candidate" ]] || continue
        if ! load_elf "$candidate"; then RESOLUTION_UNKNOWN=true; continue; fi
        if elf_arch_matches "$candidate" "$MACHINE"; then RESOLVED=$candidate; return 0; fi
    done <<< "$candidates"
    return 1
}

inspect_elf() {
    local label=$1 root=$2 path kind value extra current index=0 complete=true uncertain=false version_missing=false rpath interpreter soname version provider missing='' key options
    local -a queue=() needs=() version_needs=()
    local -A visited=() exports=() imports=() definitions=() providers=() sections=()
    [[ "$root" == /* ]] || root="$PWD/$root"
    collect "$label-hash" false sha256sum -- "$root"
    fact "$label:path" "$root"
    if [[ ! -r "$root" ]]; then
        if [[ -e "$root" ]]; then finding UNKNOWN "$label" "交付物不可读：$root" true '核对交付路径和读取权限。'
        else finding BLOCK "$label" "交付物不存在：$root" true '提供成套的正确交付物。'; fi
        return
    fi
    if [[ ! -f "$root" ]]; then
        finding BLOCK "$label.file-type" '交付物必须是普通文件或指向普通文件的链接。' true '选择正确的交付文件，不接受设备、FIFO 或 socket。'
        return
    fi
    if ! load_elf "$root"; then
        finding UNKNOWN "$label.elf" '无法分析 ELF；保留错误，不执行该文件。' true
        return
    fi
    local expected=$MACHINE
    [[ "$label" != bpf-object ]] || expected=bpf
    if [[ -z "${ELF_MACHINE[$root]:-}" ]]; then
        finding UNKNOWN "$label.header" 'ELF 输出格式无法解析。' true; return
    fi
    if ! elf_arch_matches "$root" "$expected"; then
        finding BLOCK "$label.arch" "交付物不是所需 64 位小端 ELF：${ELF_MACHINE[$root]:-unknown}；要求 $expected。" true \
            '在兼容环境重建；用户态 ELF 与主机架构相同，BPF ELF 是 eBPF，不能按 AArch64 判断。'
        return
    fi
    finding PASS "$label.arch" "ELF 架构符合 $expected。" false
    if [[ "$label" == bpf-object ]]; then
        [[ "${ELF_TYPE[$root]:-}" == REL ]] || finding BLOCK "$label.type" 'BPF 必须为可重定位 ELF。' true '重新构建成套交付物。'
        while IFS=$'\t' read -r kind value extra; do [[ "$kind" != S ]] || sections[$value]=1; done < "$EP"
        for key in "${LSM_HOOKS[@]}"; do [[ -n "${sections[lsm/$key]+present}" ]] || missing+=" lsm/$key"; done
        for key in .BTF .BTF.ext .maps raw_tracepoint/sched_process_fork tracepoint/syscalls/sys_enter_execve tracepoint/syscalls/sys_enter_execveat tracepoint/sched/sched_process_exec tracepoint/sched/sched_process_exit; do
            [[ -n "${sections[$key]+present}" ]] || missing+=" $key"
        done
        if [[ -n "$missing" ]]; then finding BLOCK "$label.sections" "交付物缺少必需段：$missing" true '重新构建成套交付物。'
        else finding PASS "$label.sections" '项目必需 BPF 段齐全。' false; fi
        return
    fi
    if [[ "${ELF_TYPE[$root]:-}" != EXEC && "${ELF_TYPE[$root]:-}" != DYN ]]; then
        finding BLOCK "$label.type" '用户态交付物不是可执行 ELF。' true '重新构建成套交付物。'; return
    fi
    queue=("$root")
    while ((index < ${#queue[@]})); do
        current=${queue[$index]}; index=$((index + 1))
        [[ -z "${visited[$current]+present}" ]] || continue
        visited[$current]=1
        if (( ${#visited[@]} > 256 )); then
            finding UNKNOWN "$label.closure-limit" '依赖闭包超过 256 个文件，停止此项以控制开销，继续其他检查。' true
            complete=false; break
        fi
        if ! load_elf "$current"; then complete=false; continue; fi
        if ! elf_arch_matches "$current" "$MACHINE"; then
            finding BLOCK "$label.dependency-arch" "依赖架构不匹配：$current" true '按部署环境重建或提供匹配的依赖。'
            complete=false; continue
        fi
        if [[ "$current" != "$root" ]]; then
            if [[ -z "${ELF_MOUNTS[$current]+present}" ]]; then
                ELF_MOUNTS[$current]=''
                if collect "$label.dependency-mount" true findmnt -n -o OPTIONS -T "$current"; then
                    IFS= read -r options < "$CF" || :
                    ELF_MOUNTS[$current]=$options
                fi
            fi
            options=${ELF_MOUNTS[$current]}
            if [[ ",$options," == *,noexec,* ]]; then
                finding BLOCK "$label.dependency-noexec" "动态加载器或依赖库所在挂载为 noexec：$current" true \
                    '由操作人员核对 findmnt 并评审兼容路径/依赖；不自动 remount 或复制系统库。'
            fi
        fi
        needs=() rpath='' interpreter=''
        while IFS=$'\t' read -r kind value extra; do
            case "$kind" in
                E) exports[$value]=1 ;; R) imports[$value]=1 ;;
                V) definitions["$current|$value"]=1 ;;
                N) version_needs+=("$current" "$value" "$extra") ;;
                D) needs+=("$value") ;;
                I) interpreter=$value ;;
                P) rpath=$value ;;
            esac
        done < "$EP"
        if [[ -n "$rpath" ]]; then
            uncertain=true
            finding UNKNOWN "$label.custom-search" "存在 RPATH/RUNPATH：$rpath；需确认最终部署位置与加载器行为。" true
        fi
        if [[ -n "$interpreter" ]]; then
            if [[ -f "$interpreter" ]]; then queue+=("$interpreter")
            else
                report_missing_dependency "$label.interpreter" "${interpreter##*/}()(64bit)" "缺少动态加载器：$interpreter" true
                complete=false
            fi
        fi
        for soname in "${needs[@]}"; do
            if resolve_library "$soname" "${current%/*}" "$rpath"; then
                queue+=("$RESOLVED"); providers["$current|$soname"]=$RESOLVED
            else
                complete=false
                local verified=false
                [[ "$CACHE_OK" != true || "$uncertain" == true || "$RESOLUTION_UNKNOWN" == true ]] || verified=true
                report_missing_dependency "$label.needed:$soname" "$soname()(64bit)" "无法在默认加载路径解析 $soname（所需文件 $current）" "$verified"
            fi
        done
    done
    missing=''
    for key in "${!imports[@]}"; do [[ -n "${exports[$key]+present}" ]] || missing+=" $key"; done
    for ((index=0; index<${#version_needs[@]}; index+=3)); do
        current=${version_needs[$index]}; soname=${version_needs[$((index + 1))]}; version=${version_needs[$((index + 2))]}
        provider=${providers["$current|$soname"]:-}
        if [[ -n "$provider" && -n "${ELF_FILES[$provider]:-}" && -z "${definitions["$provider|$version"]+present}" ]]; then
            missing+=" $soname:$version"
            version_missing=true
            report_missing_dependency "$label.abi:$soname:$version" "$soname($version)(64bit)" "现有库 $provider 缺少必需符号版本 $version" true
        fi
    done
    if [[ -n "$missing" ]]; then
        local status=UNKNOWN
        [[ "$complete" != true || "$uncertain" == true ]] || status=BLOCK
        if [[ "$complete" != true || "$version_missing" == true ]]; then status=WARN; fi
        finding "$status" "$label.symbols" "已解析依赖中缺少必需符号/版本：$missing" true \
            '依赖闭包不完整时先补齐相关依赖再复查；现有库 ABI 不兼容时，优先提供适配现有库的构建，或核对随包候选 RPM 是否提供所需符号版本。' \
            '仅同名 RPM 不代表能提供所需 ABI；确认候选包的版本化 Provides 和安装事务后再处理。'
    else
        finding INFO "$label.symbols" '已扫描可解析依赖及符号/版本，原始输出列入部署环境报告；实际加载器环境需在部署验收时复核。' false
    fi
    for path in "${!visited[@]}"; do fact "$label:dependency" "$path"; done
}

missing_input() {
    local item=$1 option=$2 purpose=$3
    finding UNKNOWN "delivery-inputs:$item" "未找到$purpose；已按交付包相对路径查找：${INPUT_SEARCHED[$item]:-无候选路径}" true \
        "确认交付包已完整解包，或用 $option 指定实际路径后重新执行检查。" \
        "若交付包确实缺少该文件，在准备机补齐交付物并更新校验清单后重新交付。"
}

check_artifacts() {
    local line soname path
    [[ -n "$DAEMON" ]] || missing_input daemon --daemon ' daemon 交付物'
    [[ -n "$CTL" ]] || missing_input ctl --ctl ' ctl 交付物'
    [[ -n "$BPF_OBJECT" ]] || missing_input bpf-object --bpf-object ' BPF 对象'
    if [[ -z "$DAEMON" && -z "$CTL" && -z "$BPF_OBJECT" ]]; then
        finding UNKNOWN delivery-inputs '没有可用交付物，不能确认交付兼容性；其余环境检查已完成。' true
    fi
    if [[ -n "$DAEMON" || -n "$CTL" ]]; then
        if collect ldconfig-cache true ldconfig -p; then
            CACHE_OK=true
            while IFS= read -r line; do
                if [[ "$line" =~ ^[[:space:]]*([^[:space:]]+)[[:space:]]+\(.*\)[[:space:]]+\=\>[[:space:]]+(.+)$ ]]; then
                    soname=${BASH_REMATCH[1]}; path=${BASH_REMATCH[2]}
                    LIBRARIES[$soname]+="$path"$'\n'
                fi
            done < "$CF"
        fi
    fi
    [[ -z "$DAEMON" ]] || inspect_elf daemon "$DAEMON"
    [[ -z "$CTL" ]] || inspect_elf ctl "$CTL"
    [[ -z "$BPF_OBJECT" ]] || inspect_elf bpf-object "$BPF_OBJECT"
    if [[ -n "$CONFIG" ]]; then
        if read_file "$CONFIG"; then
            finding INFO config-validation '已保存显式提供的配置；不执行交付程序，也不以简化 YAML 解析替代语义校验。' false \
                '由操作人员确认来源可信后执行：censorguardd launch --config /etc/censorguard/base.yaml --check-config' \
                '首次部署应为 permissive、自保护/全量审计/DSH/gRPC 关闭、空规则；详见部署指南。'
        else finding UNKNOWN config '指定配置不可读，先核对路径/权限。' true; fi
    else
        finding WARN config-not-found '未找到待检查的配置示例；已按交付包相对路径查找。' false \
            "查找路径：${INPUT_SEARCHED[config]:-无候选路径}" \
            '确认交付包含 etc/censorguard/base.yaml.example，或用 --config 指定。'
    fi
}
