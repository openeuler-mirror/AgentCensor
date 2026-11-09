check_tools() {
    local specification tool package purpose status candidate required
    for specification in \
        'rpm:rpm:部署' 'dnf:dnf:部署' 'systemctl:systemd:部署' \
        'systemd-sysusers:systemd:部署' 'systemd-tmpfiles:systemd:部署' 'tar:tar:部署' 'gzip:gzip:部署' \
        'timeout:coreutils:诊断' 'cat:coreutils:诊断' 'uname:coreutils:诊断' 'getconf:glibc-common:诊断' \
        'findmnt:util-linux:诊断' 'df:coreutils:诊断' 'awk:gawk:诊断' 'readelf:binutils:诊断' \
        'ldconfig:glibc:诊断' 'bpftool:bpftool:诊断'; do
        IFS=: read -r tool package purpose <<< "$specification"
        if command -v "$tool" >/dev/null 2>&1; then
            fact "command:$tool" "$(command -v "$tool")"
            continue
        fi
        candidate=${BUNDLE_PACKAGES[$package]:-}
        status=UNKNOWN required=true
        if [[ -n "$candidate" ]]; then
            status=MISSING required=false
        elif [[ "$purpose" == 部署 && "$BUNDLE_COMPLETE" == true ]]; then
            status=BLOCK
        fi
        if [[ -n "$candidate" ]]; then
            local candidate_path
            candidate_path=$(first_candidate_path "$candidate")
            install_commands "$package" "$candidate_path"
            FINDING_CANDIDATE=$candidate_path
            finding "$status" "command:$tool" "$purpose 命令 $tool 缺失；候选包 $package；随包候选：$candidate_path。" "$required" \
                "先核对已安装状态：rpm -q '$package' && rpm -ql '$package'" \
                "核验候选 RPM：$INSTALL_VERIFY_COMMAND" \
                "先预览安装事务：$INSTALL_PREVIEW_COMMAND" \
                "确认预览结果后安装：$INSTALL_COMMAND" \
                '安装完成后重新运行环境检查（同一报告目录会替换上一轮报告）。'
        else
            FINDING_CANDIDATE=''
            finding "$status" "command:$tool" "$purpose 命令 $tool 缺失；候选包 $package；随包候选：未找到。" "$required" \
                "先核对已安装状态：rpm -q '$package' && rpm -ql '$package'" \
                "在准备机补充 '$package' 及其依赖闭包后交付，再重新检查。"
        fi
    done
}

cgroup_path() {
    local root=$1 mountpoint=$2 membership=$3 relative
    root=${root//\\040/ }; root=${root//\\011/$'\t'}; root=${root//\\012/$'\n'}; root=${root//\\134/\\}
    mountpoint=${mountpoint//\\040/ }; mountpoint=${mountpoint//\\011/$'\t'}; mountpoint=${mountpoint//\\012/$'\n'}; mountpoint=${mountpoint//\\134/\\}
    [[ "$membership" == /* && "/$membership/" != */../* ]] || return 1
    if [[ "$root" == / ]]; then relative=$membership
    elif [[ "$membership" == "$root" ]]; then relative=''
    elif [[ "$membership" == "$root/"* ]]; then relative=${membership#"$root"}
    elif [[ "$membership" == / ]]; then relative=''
    else return 1
    fi
    CGROUP_PATH=${mountpoint%/}$relative
    CGROUP_PATH=${CGROUP_PATH%/}
}

check_cgroups() {
    local mountinfo=$1 memberships=$2 hierarchy controllers membership controller line separator fs options
    local unified='' v1=false v2=false mode directory file version
    local -a fields=() files=()
    local -A groups=() paths=() versions=()
    if [[ ! -r "$mountinfo" || ! -r "$memberships" ]]; then
        finding WARN cgroup-detection '无法读取 cgroup 挂载或当前进程归属信息，跳过资源限制采集。' false
        return
    fi
    while IFS=: read -r hierarchy controllers membership; do
        if [[ "$hierarchy" == 0 && -z "$controllers" ]]; then unified=$membership; fi
        for controller in memory cpu pids; do
            [[ ",$controllers," != *",$controller,"* ]] || groups[$controller]=$membership
        done
    done < "$memberships"
    while IFS= read -r line; do
        read -r -a fields <<< "$line"
        ((${#fields[@]} >= 10)) || continue
        separator=6
        while ((separator < ${#fields[@]})) && [[ "${fields[$separator]}" != - ]]; do separator=$((separator + 1)); done
        ((separator + 3 < ${#fields[@]})) || continue
        fs=${fields[$((separator + 1))]}; options=${fields[$((separator + 3))]}
        case "$fs" in
            cgroup) v1=true ;;
            cgroup2) v2=true ;;
            *) continue ;;
        esac
        for controller in memory cpu pids; do
            if [[ "$fs" == cgroup ]]; then
                [[ ",$options," == *",$controller,"* && -n "${groups[$controller]:-}" ]] || continue
                membership=${groups[$controller]}; version=v1
            else
                [[ -n "$unified" && "${versions[$controller]:-}" != v1 ]] || continue
                membership=$unified; version=v2
            fi
            if cgroup_path "${fields[3]}" "${fields[4]}" "$membership"; then
                paths[$controller]=$CGROUP_PATH; versions[$controller]=$version
            fi
        done
    done < "$mountinfo"
    if [[ "$v1" == true && "$v2" == true ]]; then mode=hybrid
    elif [[ "$v1" == true ]]; then mode=v1
    elif [[ "$v2" == true ]]; then mode=v2
    else mode=unavailable
    fi
    fact cgroup_mode "$mode"
    finding INFO cgroup-scope '资源限制取自检查进程所属的 cgroup；服务实际限制及祖先 cgroup 的约束需在运行验收时复核。' false
    for controller in memory cpu pids; do
        directory=${paths[$controller]:-}; version=${versions[$controller]:-}
        if [[ -z "$directory" ]]; then
            finding INFO "cgroup:$controller" '没有可解析的控制器挂载和进程归属，跳过可选资源限制采集。' false
            continue
        fi
        fact "cgroup:$controller" "$version $directory"
        case "$version:$controller" in
            v1:memory) files=(memory.limit_in_bytes memory.usage_in_bytes) ;;
            v1:cpu) files=(cpu.cfs_quota_us cpu.cfs_period_us) ;;
            v2:memory) files=(memory.max memory.current) ;;
            v2:cpu) files=(cpu.max) ;;
            *:pids) files=(pids.max) ;;
        esac
        for file in "${files[@]}"; do
            if [[ -e "$directory/$file" ]]; then read_file "$directory/$file"
            else finding INFO "cgroup:$controller:$file" "当前 cgroup 未暴露可选接口 $directory/$file；不据此判定部署阻断。" false
            fi
        done
    done
}

check_identity() {
    local path line caps pid1='' key value mountinfo='' memberships=''
    collect uname true uname -a
    if collect architecture true uname -m; then
        IFS= read -r MACHINE < "$CF" || :
        fact architecture "$MACHINE"
        if [[ "$ARCH" != any && "$ARCH" != "$MACHINE" ]]; then
            finding BLOCK architecture "实际架构 $MACHINE，本次交付目标 $ARCH。" true '选择对应架构的交付物或兼容主机。'
        else
            finding PASS architecture "实际架构 $MACHINE；不限制 OS/RPM 修订版。" false
        fi
    fi
    collect kernel-release true uname -r
    if ((RC == 0)); then IFS= read -r RELEASE < "$CF" || :; fi
    fact kernel_release "$RELEASE"
    fact euid "$EUID"
    fact bash "$BASH_VERSION"
    printf -v value '%(%Y-%m-%dT%H:%M:%S%z)T' -1; fact timestamp "$value"
    ((EUID == 0)) || finding WARN privilege '当前非 root，权限不足的检查会标为 UNKNOWN。' false \
        'sudo bash check-deployment-env.sh --output /var/tmp/cg-deployment-check（以 root 重新执行；交付物按相对路径自动识别）'
    for path in /etc/os-release /etc/openEuler-release /etc/nsswitch.conf /etc/passwd /etc/group /etc/ld.so.conf \
        /etc/hostname /proc/version /proc/cmdline /proc/modules /proc/cpuinfo /proc/meminfo \
        /proc/loadavg /proc/uptime /proc/filesystems /proc/self/mountinfo /proc/self/cgroup \
        /proc/self/status /proc/self/limits /proc/1/comm /proc/1/status /proc/1/cgroup \
        /sys/devices/system/cpu/online /sys/devices/system/cpu/possible \
        /proc/sys/kernel/unprivileged_bpf_disabled /proc/sys/kernel/perf_event_paranoid \
        /proc/sys/kernel/yama/ptrace_scope /sys/kernel/security/lockdown \
        /proc/sys/net/core/bpf_jit_enable /proc/sys/net/core/bpf_jit_harden \
        /sys/fs/selinux/enforce /etc/selinux/config /etc/ld.so.conf.d/*.conf; do
        [[ "$path" != *'*'* ]] || continue
        read_file "$path"
        if ((RC == 0)); then
            case "$path" in
                /proc/self/mountinfo) mountinfo=$CF ;;
                /proc/self/cgroup) memberships=$CF ;;
            esac
        fi
        if [[ "$path" == /proc/1/comm && "$RC" == 0 ]]; then
            IFS= read -r pid1 < "$CF" || :
        elif [[ "$path" == /proc/self/status && "$RC" == 0 ]]; then
            while IFS= read -r line; do
                if [[ "$line" =~ ^CapBnd:[[:space:]]*([0-9a-fA-F]+)$ ]]; then
                    caps=${BASH_REMATCH[1]}; fact capability_bounding_set "$caps"
                    if ! (( (16#$caps & (1 << 21)) || ((16#$caps & (1 << 39)) && (16#$caps & (1 << 38))) )); then
                        finding UNKNOWN capabilities '当前会话缺少 SYS_ADMIN 或 BPF+PERFMON；不能据此确认宿主 systemd 服务的权限。' true \
                            '在目标宿主机以 root 复查；核对服务 CapabilityBoundingSet/容器权限，不要全局关闭安全机制。'
                    fi
                fi
            done < "$CF"
        elif [[ "$path" == /proc/sys/net/core/bpf_jit_enable && "$RC" == 0 ]]; then
            IFS= read -r value < "$CF" || :
            [[ "$value" != 0 ]] || finding WARN jit-runtime 'BPF JIT 开关为 0，请在同内核验收机评估解释执行的性能。' false \
                '只读核对：cat /proc/sys/net/core/bpf_jit_enable；不自动修改 sysctl。'
        fi
    done
    check_cgroups "$mountinfo" "$memberships"
    if [[ "$pid1" == systemd ]]; then
        finding PASS systemd 'PID 1 为 systemd，服务权限及 unit 解析仍须在安装阶段复验。' false
    elif [[ -n "$pid1" ]]; then
        finding BLOCK systemd "PID 1 为 $pid1，不能按交付的 systemd unit 部署。" true '选择 systemd 宿主机；容器不能自行补齐宿主的 BPF LSM。'
    else
        finding UNKNOWN systemd '无法确认 PID 1。' true
    fi
    collect cpu false lscpu
    collect page-size true getconf PAGESIZE
    collect rpms true rpm -qa --qf '%{NAME} %{EPOCHNUM}:%{VERSION}-%{RELEASE} %{ARCH}\n'
    collect systemd-version false systemctl --version
    collect glibc false getconf GNU_LIBC_VERSION
    collect storage false lsblk -f
    collect filesystems false df -PT
    collect inodes false df -Pi
    collect mounts false findmnt -A
    collect virtualization false systemd-detect-virt
    collect selinux false getenforce
    collect secureboot false mokutil --sb-state
    collect interfaces false ip -details address show
    collect routes false ip route show table all
    collect routes-ipv6 false ip -6 route show table all
    collect listeners false ss -lntu
    collect project-unit false systemctl show censorguardd.service --no-pager \
        --property=LoadState,ActiveState,SubState,MainPID,FragmentPath,DropInPaths,User,Group,LimitMEMLOCK,LimitNOFILE,CapabilityBoundingSet
    for path in /etc/yum.repos.d/*.repo; do
        [[ -e "$path" ]] && collect repository-hash false sha256sum -- "$path"
    done
    finding INFO security-policy '已记录 SELinux/lockdown/Secure Boot/seccomp/BPF sysctl；开启这些机制本身不直接判阻断。仓库仅记录文件路径及哈希，避免导出仓库凭据。' false
}

check_kernel() {
    local path config='' line key value lsm='' dump='' parsed='' missing='' kind name
    local -A configs=() funcs=() enums=() events=()
    for path in /proc/config.gz "/boot/config-$RELEASE" "/lib/modules/$RELEASE/config"; do
        read_file "$path"
        ((RC == 0)) || continue
        if [[ "$path" == *.gz ]]; then
            collect kernel-config-decompress false gzip -dc -- "$CF"
            ((RC == 0)) || continue
        fi
        config=$CF
        break
    done
    if [[ -z "$config" ]]; then
        finding UNKNOWN kernel-config '无法读取当前运行内核配置；不以内核版本号猜测。' true \
            'sudo cat /boot/config-"$(uname -r)"；或向供应商取得运行内核的配置，不能使用其他内核配置代替。'
    else
        collect kernel-config-export true cat -- "$config"
        if ((RC == 0)); then
            if ! cat -- "$CF" > "$OUT/raw/kernel.config"; then WRITE_ERROR=1; fi
        fi
        while IFS= read -r line; do
            if [[ "$line" =~ ^CONFIG_([A-Z0-9_]+)=(.*)$ ]]; then
                configs[${BASH_REMATCH[1]}]=${BASH_REMATCH[2]}
            elif [[ "$line" =~ ^\#[[:space:]]CONFIG_([A-Z0-9_]+)[[:space:]]is[[:space:]]not[[:space:]]set$ ]]; then
                configs[${BASH_REMATCH[1]}]=n
            fi
        done < "$config"
        if [[ -z "${configs[BPF]+present}" ]]; then
            finding UNKNOWN kernel-config-format '读取内容未包含 CONFIG_BPF，无法确认是完整配置。' true
        else
            for key in BPF BPF_SYSCALL BPF_LSM BPF_EVENTS DEBUG_INFO_BTF SECURITY SECURITY_PATH FTRACE_SYSCALLS TRACEPOINTS PERF_EVENTS; do
                value=${configs[$key]:-n}
                if [[ "$value" == y ]]; then
                    finding PASS "CONFIG_$key" "$key=$value" false
                else
                    kernel_remedy BLOCK "CONFIG_$key" "$key=$value，缺少必需内核配置。"
                fi
            done
            [[ "${configs[BPF_JIT]:-n}" == y ]] || finding WARN jit '未确认 BPF JIT 编译支持，请先验收性能。' false
        fi
    fi
    read_file /sys/kernel/security/lsm
    if ((RC != 0)); then
        finding UNKNOWN active-lsm '无法读取活动 LSM，可能是权限或 securityfs 未挂载。' true \
            'sudo cat /sys/kernel/security/lsm; findmnt -t securityfs' \
            '若确认内核支持且部署方确认挂载：sudo mount -t securityfs securityfs /sys/kernel/security；这不能启用当前未激活的 BPF LSM。'
    else
        IFS= read -r lsm < "$CF" || :
        if [[ ",$lsm," == *,bpf,* ]]; then
            finding PASS active-lsm "$lsm" false
        else
            kernel_remedy BLOCK active-lsm "活动 LSM 列表没有 bpf：$lsm；当前启动期间没有运行时启用开关。"
        fi
    fi
    read_file /sys/kernel/btf/vmlinux
    if ((RC != 0)); then
        if [[ ! -e /sys/kernel/btf/vmlinux && -r /sys/kernel/btf && -x /sys/kernel/btf ]]; then
            kernel_remedy BLOCK btf '标准路径缺少运行内核 BTF，当前交付的 unit/程序不能部署。'
        else
            finding UNKNOWN btf '无法取得运行内核 BTF，先区分权限、sysfs 挂载和能力缺失。' true \
                'sudo ls -l /sys/kernel/btf/vmlinux; findmnt -t sysfs'
        fi
    else
        collect btf-export true cp -- "$CF" "$OUT/raw/vmlinux.btf"
        if ((RC == 0)); then
            collect btf-hash false sha256sum -- "$OUT/raw/vmlinux.btf"
            if collect btf-dump true bpftool btf dump file "$OUT/raw/vmlinux.btf" format raw; then
                dump=$CF
                if collect btf-parse true awk '
                    /^\[[0-9]+\] FUNC / { n=$3; gsub(/\047/, "", n); print "F\t" n; next }
                    /^\[[0-9]+\]/ { enum=($2=="ENUM" || $2=="ENUM64"); next }
                    enum && /^[ \t]*\047/ { n=$1; gsub(/\047/, "", n); print "E\t" n }
                    ' "$dump"; then
                    parsed=$CF
                    while IFS=$'\t' read -r kind name; do
                        [[ -n "$name" ]] || continue
                        case "$kind" in F) funcs[$name]=1 ;; E) enums[$name]=1 ;; esac
                    done < "$parsed"
                    if ((${#funcs[@]} == 0)); then
                        finding UNKNOWN btf-format 'BTF 输出未解析出函数，不能将其误报为所有 hook 缺失。' true
                    else
                        for key in "${LSM_HOOKS[@]}"; do
                            [[ -n "${funcs[bpf_lsm_$key]+present}" ]] || missing+=" $key"
                        done
                        if [[ -n "$missing" ]]; then
                            kernel_remedy BLOCK lsm-hooks "完整 BTF 中缺少挂载目标：$missing"
                        else
                            finding PASS lsm-hooks "全部 ${#LSM_HOOKS[@]} 个必需 LSM 挂载目标存在。" false
                        fi
                    fi
                    for key in BPF_FUNC_loop BPF_FUNC_ringbuf_reserve BPF_FUNC_ringbuf_submit BPF_MAP_TYPE_RINGBUF BPF_MAP_TYPE_ARRAY_OF_MAPS; do
                        if [[ -n "${enums[$key]+present}" ]]; then
                            finding INFO "$key" 'BTF 中存在声明，具体程序是否可用仍须 verifier 验证。' false
                        else
                            finding UNKNOWN "$key" 'BTF 未提供该声明，不能静态确认可用。' true
                        fi
                    done
                fi
            fi
        fi
    fi
    dump=''
    for path in /sys/kernel/tracing/available_events /sys/kernel/debug/tracing/available_events; do
        read_file "$path"
        if ((RC == 0)); then dump=$CF; break; fi
    done
    if [[ -z "$dump" ]]; then
        finding UNKNOWN tracepoints '无法读取 tracefs available_events；可能未挂载或无权限。' true \
            'findmnt -t tracefs; sudo cat /sys/kernel/tracing/available_events' \
            '若内核支持、目录存在且部署方确认：sudo mount -t tracefs tracefs /sys/kernel/tracing；然后重新检查。'
    else
        while IFS= read -r line; do events[$line]=1; done < "$dump"
        for key in syscalls:sys_enter_execve syscalls:sys_enter_execveat sched:sched_process_exec sched:sched_process_exit; do
            if [[ -n "${events[$key]+present}" ]]; then
                finding PASS "$key" 'tracepoint 存在。' false
            else
                kernel_remedy BLOCK "$key" '完整 available_events 中缺少必需 tracepoint。'
            fi
        done
    fi
    finding INFO runtime-boundary '未加载 BPF；CO-RE、helper/map 授权、raw sched_process_fork、签名/verifier 和运行性能须在同内核验收机验证。缺少 bpftool 仅造成静态信息缺口，不代表缺少运行能力。' false
}

check_storage() {
    local destination parent line options available total path
    for destination in /usr/sbin /usr/bin /usr/lib/censorguard /etc/censorguard /run/censorguard /var/lib/censorguard; do
        parent=$destination
        while [[ ! -d "$parent" && "$parent" != / ]]; do parent=${parent%/*}; [[ -n "$parent" ]] || parent=/; done
        if collect "mount:$destination" true findmnt -n -o OPTIONS -T "$parent"; then
            IFS= read -r options < "$CF" || :
            if [[ ",$options," == *,ro,* ]]; then
                finding BLOCK "storage:$destination" '默认部署目录所在挂载为只读。' true \
                    "只读核对：findmnt -T '$parent'; df -h '$parent'" \
                    '由操作人员决定路径/unit 调整或批准的挂载变更；不自动 remount。'
            fi
            if [[ "$destination" == /usr/bin || "$destination" == /usr/sbin ]] && [[ ",$options," == *,noexec,* ]]; then
                finding BLOCK "noexec:$destination" '默认二进制安装目录为 noexec。' true \
                    '由操作人员选择允许执行的路径并相应调整 unit/CLI；不自动 remount。'
            fi
        fi
        if collect "space:$destination" true df -k --output=avail -- "$parent"; then
            available=''
            while IFS= read -r line; do [[ "$line" =~ ^[[:space:]]*([0-9]+)[[:space:]]*$ ]] && available=${BASH_REMATCH[1]}; done < "$CF"
            fact "space_kib:$destination" "$available"
            if [[ "$available" == 0 ]]; then
                finding BLOCK "space:$destination" '文件系统已无可用空间。' true \
                    "只读核对：df -h '$parent'；由操作人员评估清理、扩容或其他路径，不自动删除文件。"
            elif [[ -z "$available" ]]; then finding UNKNOWN "space:$destination" '无法解析空间信息。' true; fi
        fi
        if collect "inodes:$destination" true df --output=itotal,iavail -- "$parent"; then
            total='' available=''
            while read -r line options; do
                if [[ "$line" =~ ^[0-9]+$ && "$options" =~ ^[0-9]+$ ]]; then total=$line; available=$options; fi
            done < "$CF"
            fact "inodes_total_free:$destination" "$total $available"
            if [[ -n "$total" && "$total" != 0 && "$available" == 0 ]]; then
                finding BLOCK "inodes:$destination" '文件系统已无可用 inode。' true '由操作人员评估清理/扩容/其他路径，不自动删除文件。'
            fi
        fi
    done
    for path in /run/censorguard /var/lib/censorguard /etc/censorguard /usr/sbin/censorguardd /usr/bin/censorguardctl; do
        if [[ -e "$path" || -L "$path" ]]; then
            collect existing-install false ls -ld -- "$path"
            finding WARN "existing:$path" '已存在，安装前核对活动实例/旧策略/socket/属主；不自动覆盖或停止。' false
        fi
    done
}
