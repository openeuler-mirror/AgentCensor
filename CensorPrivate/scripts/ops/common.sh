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
  jq -e 'type == "object" and (.mode|type == "string") and (.port|type == "number" and . == floor) and (.upstream|type == "string") and (.model|type == "string") and (.has_key|type == "boolean") and (.native_library|type == "string") and ((.ca_file // "")|type == "string")' "$config_dir/config.json" >/dev/null || fail '配置格式错误'
  mode=$(jq -r .mode "$config_dir/config.json")
  port=$(jq -r .port "$config_dir/config.json")
  upstream=$(jq -r .upstream "$config_dir/config.json")
  model=$(jq -r .model "$config_dir/config.json")
  validate_connection
  private_file "$config_dir/gateway.token"; private_file "$config_dir/curl.headers"; private_file "$config_dir/policy.yaml"
  [[ $(jq -r .has_key "$config_dir/config.json") == false ]] || private_file "$config_dir/upstream.key"
  ca_file=$(jq -r '.ca_file // ""' "$config_dir/config.json")
  if [[ -n $ca_file ]]; then [[ $ca_file == /* ]] || fail 'CA 文件需要绝对路径'; private_file "$ca_file"; fi
}
find_bins() {
  if [[ -f $CP_REPO_ROOT/BUNDLE.json && -x $CP_DEFAULT_BIN_DIR/censorprivacy ]]; then
    bin_dir=$CP_DEFAULT_BIN_DIR
  elif [[ -f $config_dir/install.json ]]; then
    private_file "$config_dir/install.json"
    bin_dir=$(jq -er .bin_dir "$config_dir/install.json")
  elif [[ -x $prefix/bin/censorprivacy ]]; then bin_dir=$prefix/bin
  elif [[ -x $CP_DEFAULT_BIN_DIR/censorprivacy ]]; then bin_dir=$CP_DEFAULT_BIN_DIR
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
process_matches() {
  [[ -f $config_dir/process.json ]] || return 1
  private_file "$config_dir/process.json"
  local pid tick current executable
  pid=$(jq -er .pid "$config_dir/process.json") || return 1
  tick=$(jq -er .start_ticks "$config_dir/process.json") || return 1
  [[ $pid =~ ^[0-9]+$ && -r /proc/$pid/stat ]] || return 1
  current=$(awk '{print $22}' "/proc/$pid/stat") || return 1
  executable=$(readlink "/proc/$pid/exe") || return 1
  executable=${executable%" (deleted)"}
  [[ $tick == "$current" && $executable == "$(readlink -f "$bin_dir/censorprivacy")" ]]
}
