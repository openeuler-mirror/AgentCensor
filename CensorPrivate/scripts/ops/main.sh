#!/usr/bin/env bash
set -euo pipefail
source "$CP_REPO_ROOT/scripts/ops/common.sh"
operation=${1:-help}; if [[ $# -gt 0 ]]; then shift; fi
config_dir=${CP_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/censorprivacy/managed}
prefix=${CP_INSTALL_PREFIX:-$HOME/.local}
prefix_explicit=false; [[ -z ${CP_INSTALL_PREFIX:-} ]] || prefix_explicit=true
tls=false; demo=false; dsh=false; runtime=false; dry_run=false; probe_model=false; no_key=false
dsh_root=${DSH_ROOT:-}; mode=gateway; upstream=; model=; key_file=; port=8775
xiaoo_root=${XIAOO_ROOT:-}; xiaoo_bin=
ner_threads=; ner_device=; ner_batch_size=; xiaoo_tools=false; tools_option=
deployment_config=${CP_CONFIG_FILE:-}
no_deployment_config=false
native_library=; ca_file=; web_port=3080; rest=(); options_seen=()
while [[ $# -gt 0 ]]; do
  [[ $1 != --* || $1 == -- ]] || options_seen+=("$1")
  case "$1" in
    --config-dir) config_dir=${2:?需要目录}; shift 2 ;;
    --config) deployment_config=${2:?需要 YAML 配置文件}; shift 2 ;;
    --no-config) no_deployment_config=true; shift ;;
    --prefix) prefix=${2:?需要绝对路径}; prefix_explicit=true; shift 2 ;;
    --dsh-path|--dsh-root) dsh_root=${2:?需要 DSH npm 安装路径或源码目录}; shift 2 ;;
    --xiaoO-path|--xiaoo-root) xiaoo_root=${2:?需要 xiaoO 源码目录}; shift 2 ;;
    --xiaoo-bin) xiaoo_bin=${2:?需要 xiaoo 可执行文件}; shift 2 ;;
    --ner-threads) ner_threads=${2:?需要 NER 线程数}; shift 2 ;;
    --ner-device) ner_device=${2:?需要 cpu、cuda[:ids] 或 cann[:ids]}; shift 2 ;;
    --ner-batch-size) ner_batch_size=${2:?需要 NER batch 大小}; shift 2 ;;
    --enable-tools|--no-tools|--disable_tool)
      requested_tools=$1; [[ $1 != --disable_tool ]] || requested_tools=--no-tools
      [[ -z $tools_option || $tools_option == "$requested_tools" ]] || fail '启用与关闭工具的参数不能同时使用'
      tools_option=$requested_tools; [[ $requested_tools != --enable-tools ]] || xiaoo_tools=true; shift ;;
    --mode) mode=${2:?需要 gateway 或 tls}; shift 2 ;;
    --upstream) upstream=${2:?需要 origin}; shift 2 ;;
    --model-id) model=${2:?需要模型 ID}; shift 2 ;;
    --key-file) key_file=${2:?需要凭据文件}; shift 2 ;;
    --port) port=${2:?需要端口}; web_port=$port; shift 2 ;;
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
    start:--config|start:--no-config) ;;
    systemd:--config|systemd:--no-config)
      [[ ${rest[0]:-install} == install ]] || fail 'systemd 的 --config/--no-config 仅用于 install' ;;
    doctor:--dsh-path|doctor:--dsh-root|demo:--dsh-path|demo:--dsh-root|dsh:--dsh-path|dsh:--dsh-root) ;;
    *:--config-dir|*:--prefix|doctor:--tls|doctor:--demo|doctor:--dsh|doctor:--runtime|deps:--tls|deps:--demo|deps:--dsh|deps:--runtime|deps:--dry-run|install:--tls|install:--dry-run|init:--mode|init:--upstream|init:--model-id|init:--key-file|init:--no-key|init:--port|init:--native-library|init:--ca-file|check:--model|systemd:--dry-run|demo:--native-library) ;;
    dsh:--port) [[ ${rest[0]:-} == web ]] || fail '--port 仅用于 dsh web' ;;
    dsh:--enable-tools|dsh:--no-tools|dsh:--disable_tool)
      [[ ${rest[0]:-} =~ ^(run|web)$ ]] || fail '工具开关仅用于 dsh run|web' ;;
    xiaoo:--xiaoO-path|xiaoo:--xiaoo-root|xiaoo:--xiaoo-bin) ;;
    init:--ner-threads|start:--ner-threads|init:--ner-device|start:--ner-device|init:--ner-batch-size|start:--ner-batch-size) ;;
    xiaoo:--enable-tools|xiaoo:--no-tools)
      [[ ${rest[0]:-} =~ ^(setup|tui|run|demo)$ ]] || fail '工具开关仅用于 xiaoo setup|tui|run|demo' ;;
    *) fail "$operation 不接受 $option" ;;
  esac
done
if $no_deployment_config; then
  [[ ! " ${options_seen[*]} " == *' --config '* ]] || fail '--config 与 --no-config 不能同时使用'
  deployment_config=
elif [[ -z $deployment_config && -f $CP_REPO_ROOT/config.yaml ]]; then deployment_config=$CP_REPO_ROOT/config.yaml; fi
if [[ -n $deployment_config && ( $operation == start || ( $operation == systemd && ${rest[0]:-install} == install ) ) ]]; then
  [[ -f $deployment_config ]] || fail '找不到 config.yaml 配置文件'
  deployment_config=$(realpath -s -- "$deployment_config")
fi
case "$operation" in demo|dsh|xiaoo|systemd|help|-h|--help) ;; *) [[ ${#rest[@]} == 0 ]] || fail '存在多余位置参数' ;; esac
case "$operation" in
  help|-h|--help)
    cat <<'HELP'
Rust 运维与演示：./censorprivacy <命令> [选项]
  doctor                 只读检查环境；可加 --tls --demo --dsh --runtime
  deps                   安装系统依赖；同上，可加 --dry-run 查看命令
  install                构建并安装 censorprivacy 统一命令；--prefix 路径、--tls
  init                   准备实例连接和凭据（已有配置不覆盖）
  start | stop           启停本用户的独立网关实例
                         start --config /path/to/config.yaml 加载统一配置
                         start --ner-device cpu|cuda[:ids]|cann[:ids]
                               --ner-threads N --ner-batch-size N 临时覆盖配置
  dashboard              显示当前网关的隐私检测台地址
  check [--model]        检查版本、认证、权限；可选真实模型请求
  systemd install|start|stop|check
                         系统服务部署与管理；install --dry-run 仅预览
  demo gateway | tls     真实本地检测器 + 模拟上游，无需云凭据
  demo dsh-gateway | dsh-tls [--dsh-path 路径]
                         真实 DSH agent + 模拟模型服务
  dsh web [--dsh-path 路径] [--port 3080]
                         启动浏览器聊天界面，复用当前网关与凭据
  dsh setup | run | cloud [--dsh-path 路径]
                         当前配置的 DSH 接入与真实模型验收
                         --dsh-root 为 --dsh-path 的兼容别名
                         路径支持 npm 安装前缀、包目录、CLI 或已构建源码；Node >=24.9
                         run/web 默认同时开放 DSH 自带工具；--disable_tool 仅保留私密工具
  xiaoo setup | tui | run | demo | web [--xiaoO-path 源码目录]
                         xiaoO 终端交互、任务、会话检测台与文件读取演示
                         默认关闭工具；--enable-tools 开启并在启动时预热
通用：--config-dir 私有目录（默认 ~/.config/censorprivacy/managed）
init：--mode gateway|tls --upstream origin --model-id ID --key-file 文件
      --port 8775；缺少连接配置时交互输入
      --no-key 本机无认证上游；--ca-file 私有 CA 文件
      --ner-device cpu|cuda[:ids]|cann[:ids]，默认 cpu；cuda/cann 默认第 0 张卡
      --ner-threads N CPU 算子线程数；--ner-batch-size N 每卡窗口数
      两项范围均为 1–256，默认 1；init 保存，已有配置不覆盖
说明：docs/usage.md；统一配置为 YAML，实例连接信息为 JSON，不执行 shell 配置。
HELP
    ;;
  doctor|deps) source "$CP_REPO_ROOT/scripts/ops/environment.sh"; "$operation" ;;
  dashboard)
    need jq; need curl
    load_config
    service_ready || fail "$readiness_error"
    [[ $(jq -r '.conversation_web // .xiaoo_web // false' <<< "$ready_response") == true ]] || fail '当前网关未启用会话检测台；更新 censorprivacy 后执行 ./censorprivacy stop 与 ./censorprivacy start'
    note "隐私检测台：http://127.0.0.1:$port/"
    ;;
  verify)
    [[ ${#rest[@]} == 0 && -n ${CP_BUNDLE_PREFIX:-} ]] || fail 'verify 仅用于完整交付包'
    (cd -- "$CP_BUNDLE_PREFIX"; sha256sum --check --quiet SHA256SUMS)
    note '通过：交付包 SHA-256 文件完整性检查。'
    ;;
  self-test)
    [[ ${#rest[@]} == 0 && -n ${CP_BUNDLE_PREFIX:-} ]] || fail 'self-test 仅用于完整交付包'
    "$CP_DEFAULT_BIN_DIR/censorprivacy" verify
    "$CP_DEFAULT_BIN_DIR/censorprivacy" doctor --tls --demo
    "$CP_DEFAULT_BIN_DIR/censorprivacy" demo gateway
    "$CP_DEFAULT_BIN_DIR/censorprivacy" demo tls
    note '通过：完整交付包自检（本地真实 NER、网关、TLS；无云调用）。'
    ;;
  xiaoo)
    need jq; need curl
    source "$CP_REPO_ROOT/scripts/ops/xiaoo.sh"
    op_xiaoo
    ;;
  install|init|start|stop|check|systemd)
    source "$CP_REPO_ROOT/scripts/ops/deploy.sh"
    need jq; need flock
    if ! $dry_run; then
      if [[ $operation == install || $operation == init ]]; then private_dir "$config_dir"
      else
        [[ -d $config_dir && ! -L $config_dir && $(stat -c %u "$config_dir") == "$EUID" && $(stat -c %a "$config_dir") == 700 ]] || fail '需要当前用户所有的 0700 配置目录；先运行 ./censorprivacy init'
      fi
      outputs=(operations.lock)
      if [[ $operation == stop ]]; then outputs+=(install.json process.json)
      elif [[ $operation != systemd || ${rest[0]:-install} != stop ]]; then
        outputs+=(install.json config.json gateway.token upstream.key upstream-ca.crt curl.headers process.json run.json service.log ready.json probe.json probe-response.json system-ready.json)
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
      --dsh-root "$dsh_root" --native-library "$native_library" --web-port "$web_port" \
      --tools "$tools_option" -- "$operation" "${rest[@]}"
    ;;
  *) fail "未知命令：$operation；运行 ./censorprivacy help" ;;
esac
