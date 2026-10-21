#!/usr/bin/env bash
set -euo pipefail
source "$CP_REPO_ROOT/scripts/ops/common.sh"
operation=${1:-help}; if [[ $# -gt 0 ]]; then shift; fi
config_dir=${CP_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/censorprivacy/managed}
prefix=${CP_INSTALL_PREFIX:-$HOME/.local}
tls=false; demo=false; dsh=false; runtime=false; dry_run=false; probe_model=false; no_key=false
dsh_root=${DSH_ROOT:-}; mode=gateway; upstream=; model=; key_file=; port=8775
native_library=; ca_file=; rest=(); options_seen=()
while [[ $# -gt 0 ]]; do
  [[ $1 != --* || $1 == -- ]] || options_seen+=("$1")
  case "$1" in
    --config-dir) config_dir=${2:?需要目录}; shift 2 ;;
    --prefix) prefix=${2:?需要绝对路径}; shift 2 ;;
    --dsh-root) dsh_root=${2:?需要 DSH 源码目录}; shift 2 ;;
    --mode) mode=${2:?需要 gateway 或 tls}; shift 2 ;;
    --upstream) upstream=${2:?需要 origin}; shift 2 ;;
    --model-id) model=${2:?需要模型 ID}; shift 2 ;;
    --key-file) key_file=${2:?需要凭据文件}; shift 2 ;;
    --port) port=${2:?需要端口}; shift 2 ;;
    --native-library) native_library=${2:?需要库路径}; shift 2 ;;
    --ca-file) ca_file=${2:?需要 CA 文件路径}; shift 2 ;;
    --no-key) no_key=true; shift ;;
    --tls) tls=true; shift ;;
    --demo) demo=true; shift ;;
    --dsh) dsh=true; demo=true; shift ;;
    --runtime) runtime=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    --model) probe_model=true; shift ;;
    --) shift; rest+=("$@"); break ;;
    -*) fail "未知参数：$1" ;;
    *) rest+=("$1"); shift ;;
  esac
done
[[ $config_dir == /* && $prefix == /* ]] || fail '配置目录与安装前缀需要绝对路径'
for option in "${options_seen[@]}"; do
  case "$operation:$option" in
    *:--config-dir|*:--prefix|doctor:--tls|doctor:--demo|doctor:--dsh|doctor:--runtime|doctor:--dsh-root|deps:--tls|deps:--demo|deps:--dsh|deps:--runtime|deps:--dry-run|install:--tls|install:--dry-run|init:--mode|init:--upstream|init:--model-id|init:--key-file|init:--no-key|init:--port|init:--native-library|init:--ca-file|check:--model|systemd:--dry-run|demo:--dsh-root|demo:--native-library|dsh:--dsh-root) ;;
    *) fail "$operation 不接受 $option" ;;
  esac
done
case "$operation" in demo|dsh|systemd|help|-h|--help) ;; *) [[ ${#rest[@]} == 0 ]] || fail '存在多余位置参数' ;; esac
case "$operation" in
  help|-h|--help)
    cat <<'HELP'
Rust 运维与演示：./cpctl <命令> [选项]
  doctor                 只读检查环境；可加 --tls --demo --dsh --runtime
  deps                   安装系统依赖；同上，可加 --dry-run 查看命令
  install                构建并安装五个 Rust 命令；--prefix 路径、--tls
  init                   准备配置、策略和凭据（已有配置不覆盖）
  start | stop           启停本用户的独立网关实例
  check [--model]        检查版本、认证、权限；可选真实模型请求
  systemd install|start|stop|check
                         系统服务部署与管理；install --dry-run 仅预览
  demo gateway | tls     本机功能演示，无需模型或云凭据
  demo dsh-gateway | dsh-tls [--dsh-root 路径]
                         真实 DSH agent + 模拟模型服务
  dsh setup | run | cloud [--dsh-root 路径]
                         当前配置的 DSH 接入与真实模型验收
通用：--config-dir 私有目录（默认 ~/.config/censorprivacy/managed）
init：--mode gateway|tls --upstream origin --model-id ID --key-file 文件
      --port 8775；缺少连接配置时交互输入
      --no-key 本机无认证上游；--ca-file 私有 CA 文件
说明：docs/usage.md；所有配置读取为 JSON，不执行 shell 配置。
HELP
    ;;
  doctor|deps) source "$CP_REPO_ROOT/scripts/ops/environment.sh"; "$operation" ;;
  install|init|start|stop|check|systemd)
    source "$CP_REPO_ROOT/scripts/ops/deploy.sh"
    need jq; need flock
    if ! $dry_run; then
      if [[ $operation == install || $operation == init ]]; then private_dir "$config_dir"
      else
        [[ -d $config_dir && ! -L $config_dir && $(stat -c %u "$config_dir") == "$EUID" && $(stat -c %a "$config_dir") == 700 ]] || fail '需要当前用户所有的 0700 配置目录；先运行 ./cpctl init'
      fi
      outputs=(operations.lock)
      if [[ $operation == stop ]]; then outputs+=(install.json process.json)
      elif [[ $operation != systemd || ${rest[0]:-install} != stop ]]; then
        outputs+=(install.json config.json policy.yaml gateway.token upstream.key upstream-ca.crt curl.headers process.json run.json service.log ready.json probe.json probe-response.json system-ready.json)
      fi
      for output in "${outputs[@]}"; do safe_output "$config_dir/$output"; done
      exec 9>"$config_dir/operations.lock"; flock -n 9 || fail '同一实例已有运维命令正在运行'
    fi
    "op_$operation"
    ;;
  demo|dsh)
    need node; need jq; find_bins
    [[ $operation != demo || ${#rest[@]} == 1 ]] || fail 'demo 需要 gateway|tls|dsh-gateway|dsh-tls 中的一项'
    exec node "$CP_REPO_ROOT/scripts/ops/demo.mjs" \
      --root "$CP_REPO_ROOT" --bin-dir "$bin_dir" --config-dir "$config_dir" \
      --dsh-root "$dsh_root" --native-library "$native_library" -- "$operation" "${rest[@]}"
    ;;
  *) fail "未知命令：$operation；运行 ./cpctl help" ;;
esac
