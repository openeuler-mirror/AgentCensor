#!/usr/bin/env bash
set -euo pipefail
umask 077
fail() { printf '错误：%s\n' "$*" >&2; exit 1; }
note() { printf '%s\n' "$*"; }
need() { command -v "$1" >/dev/null || fail "缺少 $1；先运行 ./cpctl doctor 和 ./cpctl deps"; }
admin() { if [[ $EUID == 0 ]]; then "$@"; else sudo "$@"; fi; }
execute() {
  if $dry_run; then
    if [[ $1 == admin ]]; then shift; if [[ $EUID != 0 ]]; then printf 'sudo '; fi; fi
    printf '%q ' "$@"; printf '\n'
  else "$@"; fi
}
private_dir() {
  [[ ! -L $1 ]] || fail '配置目录不能是符号链接'
  if [[ -e $1 ]]; then [[ -d $1 && $(stat -c %u "$1") == "$EUID" ]] || fail '配置目录必须是当前用户的目录'; fi
  install -d -m 0700 "$1"
  [[ $(stat -c %u "$1") == "$EUID" ]] || fail '配置目录必须属于当前用户'
}
private_file() {
  [[ -f $1 && ! -L $1 && $(stat -c %u "$1") == "$EUID" && $(stat -c %a "$1") == 600 ]] || fail "需要当前用户所有的 0600 普通文件：$1"
}
safe_output() {
  [[ ! -L $1 ]] || fail "输出文件不能是符号链接：$1"
  [[ ! -e $1 ]] || private_file "$1"
}
validate_connection() {
  [[ $mode == gateway || $mode == tls ]] || fail '模式必须为 gateway 或 tls'
  [[ $upstream =~ ^https?://([a-zA-Z0-9.-]+|\[[a-fA-F0-9:]+\])(:[0-9]{1,5})?/?$ && $upstream != *'.example'* ]] || fail '需要不含路径、查询、凭据的实际上游 origin'
  [[ $model =~ ^[a-zA-Z0-9._:/-]+$ && ${#model} -le 200 ]] || fail '模型 ID 格式错误'
  [[ $port =~ ^[1-9][0-9]{3,4}$ && $port -ge 1024 && $port -le 65535 ]] || fail '端口应为 1024–65535'
  if [[ $mode == tls ]]; then [[ $upstream =~ ^https://[0-9.]+(:[0-9]+)?/?$ ]] || fail 'TLS direct 需要 HTTPS IPv4 origin'; fi
}
load_config() {
  need jq
  private_file "$config_dir/config.json"
  jq -e 'type == "object" and (.mode|type == "string") and (.port|type == "number" and . == floor) and (.upstream|type == "string") and (.model|type == "string") and (.has_key|type == "boolean") and (.native_library|type == "string") and ((.ca_file // "")|type == "string") and ((has("ner_device")|not) or (.ner_device|type == "string")) and ((has("ner_batch_size")|not) or (.ner_batch_size|type == "number" and . == floor and . >= 1 and . <= 256)) and ((has("ner_threads")|not) or (.ner_threads|type == "number" and . == floor and . >= 1 and . <= 256))' "$config_dir/config.json" >/dev/null || fail '配置格式错误'
  mode=$(jq -r .mode "$config_dir/config.json")
  port=$(jq -r .port "$config_dir/config.json")
  upstream=$(jq -r .upstream "$config_dir/config.json")
  model=$(jq -r .model "$config_dir/config.json")
  if [[ -z $ner_threads ]]; then ner_threads=$(jq -r '.ner_threads // 1' "$config_dir/config.json"); fi
  if [[ -z $ner_device ]]; then ner_device=$(jq -r '.ner_device // "cpu"' "$config_dir/config.json"); fi
  if [[ -z $ner_batch_size ]]; then ner_batch_size=$(jq -r '.ner_batch_size // 1' "$config_dir/config.json"); fi
  validate_ner_options
  validate_connection
  private_file "$config_dir/gateway.token"; private_file "$config_dir/curl.headers"; private_file "$config_dir/policy.yaml"
  [[ $(jq -r .has_key "$config_dir/config.json") == false ]] || private_file "$config_dir/upstream.key"
  ca_file=$(jq -r '.ca_file // ""' "$config_dir/config.json")
  if [[ -n $ca_file ]]; then [[ $ca_file == /* ]] || fail 'CA 文件需要绝对路径'; private_file "$ca_file"; fi
}
validate_ner_threads() {
  [[ $ner_threads =~ ^[1-9][0-9]{0,2}$ && $ner_threads -le 256 ]] || fail 'NER 线程数应为 1–256 的整数'
}
validate_ner_options() {
  validate_ner_threads
  [[ $ner_batch_size =~ ^[1-9][0-9]{0,2}$ && $ner_batch_size -le 256 ]] || fail 'NER batch 大小应为 1–256 的整数'
  case "$ner_device" in cuda|cann) ner_device=$ner_device:0 ;; esac
  [[ $ner_device != cpu ]] || return 0
  [[ $ner_device =~ ^(cuda|cann):[0-9]+(,[0-9]+)*$ ]] || fail 'NER 设备应为 cpu、cuda[:ids] 或 cann[:ids]'
  local ids=() id normalized= seen=, number backend=${ner_device%%:*}
  IFS=, read -r -a ids <<< "${ner_device#*:}"
  [[ ${#ids[@]} -le 64 ]] || fail 'NER 设备列表最多 64 项'
  for id in "${ids[@]}"; do
    [[ ${#id} -le 10 ]] || fail 'NER 设备编号超出范围'
    number=$((10#$id))
    [[ $number -le 2147483647 ]] || fail 'NER 设备编号超出范围'
    [[ $seen != *",$number,"* ]] || fail 'NER 设备编号不能重复'
    seen+="$number,"
    normalized+="${normalized:+,}$number"
  done
  ner_device=$backend:$normalized
}

find_bins() {
  if [[ -f $CP_REPO_ROOT/BUNDLE.json && -x $CP_DEFAULT_BIN_DIR/censorprivacy ]]; then
    bin_dir=$CP_DEFAULT_BIN_DIR
  elif ${prefix_explicit:-false} && [[ -x $prefix/bin/censorprivacy ]]; then
    bin_dir=$prefix/bin
  elif [[ -f $CP_REPO_ROOT/Cargo.toml && -x $CP_REPO_ROOT/target/release/censorprivacy ]]; then
    bin_dir=$CP_REPO_ROOT/target/release
  elif [[ -x $CP_DEFAULT_BIN_DIR/censorprivacy ]]; then
    bin_dir=$CP_DEFAULT_BIN_DIR
  elif [[ -f $config_dir/install.json ]]; then
    private_file "$config_dir/install.json"
    bin_dir=$(jq -er .bin_dir "$config_dir/install.json")
  elif [[ -x $prefix/bin/censorprivacy ]]; then bin_dir=$prefix/bin
  else bin_dir=$CP_REPO_ROOT/target/release; fi
  [[ $bin_dir == /* && $bin_dir != *$'\n'* ]] || fail '无效安装路径'
  [[ -x $bin_dir/censorprivacy ]] || fail '找不到 Rust 可执行文件；先运行 ./cpctl install'
}
auth_curl() {
  curl --noproxy '*' --silent --show-error --connect-timeout 2 --max-time 90 --header "@$config_dir/curl.headers" "$@"
}
# A live process can hold an expired run. Keep the HTTP diagnosis separate from
# process ownership, which is still required before sending any signal.
service_ready() {
  local response code
  readiness_error=; ready_response=
  if ! response=$(auth_curl --max-time 5 --write-out $'\n%{http_code}' "http://127.0.0.1:$port/ready" 2>/dev/null); then
    readiness_error='无法连接当前服务；请执行 ./cpctl start；若进程仍在运行，先 ./cpctl stop 再启动'
    return 1
  fi
  code=${response##*$'\n'}
  ready_response=${response%$'\n'*}
  if [[ $code == 401 ]]; then
    readiness_error='网关认证失败（HTTP 401）；请核对 --config-dir 与当前服务的凭据'
    return 1
  fi
  if [[ $code == 400 ]] && jq -e '.error.code == "vault_denied"' <<< "$ready_response" >/dev/null 2>&1; then
    readiness_error='当前 run 已失效（vault_denied；请检查 policy.yaml 的 ttl_seconds）；执行 ./cpctl stop 后 ./cpctl start，并新建 Agent 会话'
    return 1
  fi
  if [[ $code != 200 ]] || ! jq -e '.status == "ready"' <<< "$ready_response" >/dev/null 2>&1; then
    readiness_error="服务尚未就绪（HTTP $code）；请检查 $config_dir/service.log"
    return 1
  fi
  if [[ ! -f $config_dir/run.json ]]; then
    readiness_error='缺少本实例 run.json；请核对 --config-dir；系统服务请先 ./cpctl systemd check'
    return 1
  fi
  private_file "$config_dir/run.json"
  if ! jq -e --slurpfile run "$config_dir/run.json" '.run_id == $run[0].run_id' <<< "$ready_response" >/dev/null 2>&1; then
    readiness_error='本地与服务的 run ID 不匹配；请核对 --config-dir；系统服务请先 ./cpctl systemd check'
    return 1
  fi
}
process_start_ticks() {
  local line fields
  [[ $1 =~ ^[0-9]+$ ]] || return 1
  IFS= read -r line 2>/dev/null < "/proc/$1/stat" || return 1
  # comm can contain spaces and parentheses; field 22 follows the final ') '.
  line=${line##*) }
  read -r -a fields <<< "$line"
  [[ ${fields[19]:-} =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "${fields[19]}"
}
process_matches() {
  [[ -f $config_dir/process.json ]] || return 1
  private_file "$config_dir/process.json"
  local pid tick current executable expected
  pid=$(jq -er .pid "$config_dir/process.json") || return 1
  tick=$(jq -er .start_ticks "$config_dir/process.json") || return 1
  [[ $pid =~ ^[0-9]+$ && $tick =~ ^[0-9]+$ ]] || return 1
  current=$(process_start_ticks "$pid") || return 1
  executable=$(readlink "/proc/$pid/exe" 2>/dev/null) || return 1
  executable=${executable%" (deleted)"}
  expected=$(jq -er '.executable // empty' "$config_dir/process.json") || expected=$(readlink -f "$bin_dir/censorprivacy")
  [[ $expected == /* && $tick == "$current" && $executable == "$expected" ]]
}
