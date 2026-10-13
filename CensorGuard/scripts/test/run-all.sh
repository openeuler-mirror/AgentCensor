#!/usr/bin/env bash

# run-all.sh —— 一键回归: 顺序跑全部集成测试, 末尾汇总。
#
# 用法:
#   sudo bash scripts/test/run-all.sh              # 全部 (不含 DSH)
#   sudo bash scripts/test/run-all.sh --dsh        # 含 gRPC、DSH 自注册/安装/发布包测试
#   sudo bash scripts/test/run-all.sh --web        # 另加真实 DSH 浏览器测试
#   sudo bash scripts/test/run-all.sh file exec    # 只跑名字含 file/exec 的
#
# 判定以脚本退出码为准: [FAIL] 字样可能是 ERR trap 在 set +e 区间的
# 误报, 退出码非 0 才是真失败。每个用例的完整输出在
# ${LOG_DIR}/<name>.log, 失败用例的日志尾部会在汇总时一并给出。
set -uo pipefail

# 断言依赖系统工具的英文输出 (id(1) strerror(3) 等会被 locale 本地化), 固定 C locale。
export LC_ALL=C

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
test_dir=${repo_dir}/scripts/test
log_dir=${RUN_ALL_LOG_DIR:-$(mktemp -d /tmp/censorguard-run-all.XXXXXX)}
per_case_timeout=${RUN_ALL_TIMEOUT:-300}

with_dsh=0
with_web=0
patterns=()
for arg in "$@"; do
    if [[ ${arg} == "--dsh" ]]; then
        with_dsh=1
    elif [[ ${arg} == "--web" ]]; then
        with_dsh=1
        with_web=1
    else
        patterns+=("${arg}")
    fi
done

if [[ ${EUID} -ne 0 ]]; then
    echo "需要 root: sudo bash $0" >&2
    exit 1
fi

# Collect both shell and Python regressions; new tests must not disappear from this entrypoint.
cases=("${test_dir}/startup-test.sh")
for f in "${test_dir}"/integration-*.sh "${test_dir}"/integration-*.py; do
    [[ ${f} == */integration-dsh-install.py ]] && continue
    cases+=("${f}")
done
if [[ ${with_dsh} -eq 1 ]]; then
    cases+=("${test_dir}/censorguard-dsh-attach-self.sh"
            "${test_dir}/integration-grpc.mjs"
            "${test_dir}/integration-dsh-install.py"
            "${test_dir}/verify-dsh-package.mjs")
fi
if [[ ${with_web} -eq 1 ]]; then
    cases+=("${test_dir}/integration-dsh-web.mjs")
fi

if [[ ${#patterns[@]} -gt 0 ]]; then
    filtered=()
    for c in "${cases[@]}"; do
        for p in "${patterns[@]}"; do
            if [[ $(basename "${c}") == *"${p}"* ]]; then
                filtered+=("${c}")
                break
            fi
        done
    done
    cases=("${filtered[@]}")
fi

if [[ ${#cases[@]} -eq 0 ]]; then
    echo "没有匹配的测试用例" >&2
    exit 1
fi

install -d -m 0700 "${log_dir}"
: >"${log_dir}/results.jsonl"

pass_list=()
fail_list=()
total_start=${SECONDS}

printf '%-38s %8s %8s\n' "CASE" "RESULT" "TIME"
printf '%-38s %8s %8s\n' "----" "------" "----"

for c in "${cases[@]}"; do
    name=$(basename "${c}")
    name=${name%.*}
    log="${log_dir}/${name}.log"
    start=${SECONDS}
    case ${c} in
        *.py) interpreter=python3 ;;
        *.mjs) interpreter=node ;;
        *) interpreter=bash ;;
    esac
    timeout "${per_case_timeout}" "${interpreter}" "${c}" >"${log}" 2>&1
    rc=$?
    elapsed=$((SECONDS - start))
    python3 -c 'import json, sys; print(json.dumps(dict(name=sys.argv[1], exit_code=int(sys.argv[2]), seconds=int(sys.argv[3]), log=sys.argv[4])))' \
        "${name}" "${rc}" "${elapsed}" "${log}" >>"${log_dir}/results.jsonl"
    if [[ ${rc} -eq 0 ]]; then
        printf '%-38s %8s %7ds\n' "${name}" "PASS" "${elapsed}"
        pass_list+=("${name}")
    else
        printf '%-38s %8s %7ds\n' "${name}" "FAIL(rc=${rc})" "${elapsed}"
        fail_list+=("${name}")
    fi
done

total=$((SECONDS - total_start))
echo
echo "================ 汇总 ================"
echo "通过 ${#pass_list[@]} / ${#cases[@]}, 总耗时 ${total}s, 日志: ${log_dir}"
if [[ ${#fail_list[@]} -gt 0 ]]; then
    echo "失败用例:"
    for name in "${fail_list[@]}"; do
        echo "  - ${name}  (日志尾部:)"
        tail -15 "${log_dir}/${name}.log" | sed 's/^/      /'
    done
    exit 1
fi
echo "全部通过"
