#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
用法：test-ebpf.sh [--case all|verifier|workload|exec] [--build-only]

默认编译并串行运行全部 eBPF 特权测试，非 root 用户自动使用 sudo。
  --case verifier  检查 BPF 内核 verifier 与探针挂载
  --case workload  检查真实文件、网络、IPC、stdio、信号和 exec 事件
  --case exec      检查 exec 更新 session/call、清除旧 call 标签
  --build-only     只编译并检查测试列表，不提权、不运行内核测试
  -h, --help       显示帮助

需要 Linux、Rust/Cargo、clang、libbpf 构建依赖、python3、内核 BTF
和可写 tracefs。请使用普通用户编译；只有内核测试需要 sudo。
日志保存在每次运行独立的 /tmp/censorscope-ebpf.* 目录。
EOF
}

fail() {
    printf '错误：%s\n' "$*" >&2
    exit 1
}

test_case=all
build_only=false
while (($#)); do
    case "$1" in
        --case)
            (($# >= 2)) || fail '--case 缺少参数'
            test_case=$2
            shift 2
            ;;
        --build-only) build_only=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "未知参数：$1（使用 --help 查看用法）" ;;
    esac
done
case "$test_case" in
    all) test_name= ;;
    verifier) test_name=every_compiled_program_passes_the_kernel_verifier ;;
    workload) test_name=stage3_real_file_tcp_ipc_stdio_signal_workload ;;
    exec) test_name=exec_replaces_inherited_call_and_prefers_canonical_context ;;
    *) fail "未知测试：$test_case" ;;
esac

[[ $(uname -s) == Linux ]] || fail 'eBPF 测试只能在 Linux 上运行'
for dependency in cargo python3 tee mktemp; do
    command -v "$dependency" >/dev/null || fail "缺少命令：$dependency"
done
workspace_dir=$(cd -- "$(dirname -- "$0")/.." && pwd -P)
cd -- "$workspace_dir"
log_dir=$(mktemp -d /tmp/censorscope-ebpf.XXXXXX)
trap 'printf "日志目录：%s\n" "$log_dir"' EXIT
printf '内核：%s\n工作目录：%s\n日志目录：%s\n' "$(uname -r)" "$workspace_dir" "$log_dir"

printf '\n[1/3] 编译 eBPF 测试程序\n'
if ! cargo test --locked -p ebpf_collector --test privileged_stage3 \
    --no-run --message-format=json \
    2> >(tee "$log_dir/build.log" >&2) > "$log_dir/artifacts.jsonl"; then
    fail '编译失败，请查看 build.log 和 artifacts.jsonl'
fi
test_binary=$(python3 - "$log_dir/artifacts.jsonl" <<'PY'
import json
import sys

executables = set()
with open(sys.argv[1]) as artifacts:
    for line in artifacts:
        item = json.loads(line)
        if (item.get('reason') == 'compiler-artifact'
                and item.get('target', {}).get('name') == 'privileged_stage3'
                and item.get('executable')):
            executables.add(item['executable'])
if len(executables) != 1:
    sys.exit('无法唯一确定 privileged_stage3 测试程序路径')
print(executables.pop())
PY
)
[[ -x "$test_binary" ]] || fail "测试程序不可执行：$test_binary"
set -- --ignored --nocapture --test-threads=1
if [[ -n "$test_name" ]]; then
    set -- "$@" --exact "$test_name"
fi
"$test_binary" "$@" --list > "$log_dir/tests.txt"
python3 - "$log_dir/tests.txt" <<'PY'
import sys

with open(sys.argv[1]) as listing:
    tests = [line.strip() for line in listing if line.rstrip().endswith(': test')]
if not tests:
    sys.exit('没有匹配的特权测试；请确认当前分支包含所选测试，避免零测试误报成功')
print('\n'.join(tests))
PY
if "$build_only"; then
    printf '\n编译与测试列表检查通过；未运行内核测试。\n'
    exit 0
fi

run_root() {
    if ((EUID == 0)); then
        "$@"
    else
        sudo -- "$@"
    fi
}
printf '\n[2/3] 检查内核运行环境（可能需要输入 sudo 密码）\n'
if ((EUID != 0)); then
    command -v sudo >/dev/null || fail '需要 root 权限，但没有找到 sudo'
    sudo -v || fail '无法获得 sudo 权限'
fi
if ! run_root python3 - <<'PY' 2>&1 | tee "$log_dir/environment.log"
import os
import sys

if os.getuid() != 0:
    sys.exit('collector 当前要求 root 权限')
if not os.access('/sys/kernel/btf/vmlinux', os.R_OK):
    sys.exit('缺少可读的 /sys/kernel/btf/vmlinux；请使用启用 BTF 的内核')
mounts = []
with open('/proc/self/mountinfo') as mountinfo:
    for line in mountinfo:
        fields, filesystem = line.rstrip().split(' - ', 1)
        fields = fields.split()
        if filesystem.split()[0] == 'tracefs':
            mounts.append((fields[4], fields[5].split(',')))
controls = {'/sys/kernel/tracing', '/sys/kernel/debug/tracing'}
if not any(path in controls and 'rw' in options for path, options in mounts):
    if mounts:
        sys.exit(f'tracefs 挂载位置或读写权限不满足 collector 要求：{mounts}')
    sys.exit('未挂载 tracefs。请执行 sudo mount -t tracefs tracefs /sys/kernel/tracing 后重试')
print('root、内核 BTF、可写 tracefs 检查通过')
PY
then
    fail '运行环境检查失败，请查看 environment.log'
fi

printf '\n[3/3] 串行运行 eBPF 特权测试\n'
if ! run_root "$test_binary" "$@" 2>&1 | tee "$log_dir/test.log"; then
    fail 'eBPF 测试失败，请查看 test.log 中的 verifier、挂载或事件断言错误'
fi
printf '\neBPF 所选测试全部通过。此结果不覆盖 daemon/SQLite 端到端归因或 HTTPS 明文采集。\n'
