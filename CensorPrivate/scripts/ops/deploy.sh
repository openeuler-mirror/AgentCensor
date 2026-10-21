#!/usr/bin/env bash
op_install() (
  [[ -f $CP_REPO_ROOT/Cargo.toml ]] || fail '预编译安装已含可执行文件；重新构建请从源码 checkout 执行 install'
  need cargo; need jq; need cc
  if $dry_run; then note "暂存构建并复制五个命令、cpctl 和手册到 $prefix（--tls=$tls）"; return; fi
  if [[ -f $config_dir/process.json ]]; then
    find_bins
    if process_matches; then fail '本实例正在运行；先 ./cpctl stop 再更新安装'; fi
  fi
  local stage
  stage=$(mktemp -d)
  trap 'rm -rf -- "$stage"' EXIT
  local args=(--prefix "$prefix")
  $tls && args+=(--with-native)
  DESTDIR="$stage" bash "$CP_REPO_ROOT/scripts/install.sh" "${args[@]}"
  local command
  for command in censorprivacy privacy-run privcalc censorprivacy-mcp tls-run cpctl; do
    if [[ -w $prefix || $prefix == "$HOME/"* ]]; then
      install -d -m 0755 "$prefix/bin"
      install -m 0755 "$stage$prefix/bin/$command" "$prefix/bin/$command"
    else admin install -D -m 0755 "$stage$prefix/bin/$command" "$prefix/bin/$command"; fi
  done
  if $tls; then
    if [[ -w $prefix ]]; then install -D -m 0755 "$stage$prefix/lib/censorprivacy/libcp_tls_guard.so" "$prefix/lib/censorprivacy/libcp_tls_guard.so"
    else admin install -D -m 0755 "$stage$prefix/lib/censorprivacy/libcp_tls_guard.so" "$prefix/lib/censorprivacy/libcp_tls_guard.so"; fi
  fi
  if [[ -w $prefix ]]; then install -d -m 0755 "$prefix/share"; cp -R --preserve=mode "$stage$prefix/share/censorprivacy" "$prefix/share/"
  else admin install -d -m 0755 "$prefix/share"; admin cp -R --preserve=mode "$stage$prefix/share/censorprivacy" "$prefix/share/"; fi
  private_dir "$config_dir"
  jq -n --arg bin "$prefix/bin" --arg library "$prefix/lib/censorprivacy/libcp_tls_guard.so" \
    '{bin_dir:$bin,native_library:$library}' > "$config_dir/install.json"
  "$prefix/bin/censorprivacy" --version
  note '安装完成；下一步：./cpctl demo gateway 或 ./cpctl init'
)
op_init() {
  need jq; need openssl
  private_dir "$config_dir"
  [[ ! -e $config_dir/config.json ]] || { note '已有配置，保留策略和凭据。直接 ./cpctl start；独立实例用 --config-dir。'; return; }
  if [[ -z $upstream ]]; then
    local selected_mode
    read -r -p "模式 gateway/tls [$mode]: " selected_mode; mode=${selected_mode:-$mode}
    read -r -p '实际上游 origin（不能填 .example）: ' upstream
  fi
  [[ -n $model ]] || read -r -p '实际模型 ID: ' model
  validate_connection
  find_bins
  local native=${native_library:-${bin_dir%/bin}/lib/censorprivacy/libcp_tls_guard.so}
  if [[ -z $native_library && ! -f $native ]]; then native=$CP_REPO_ROOT/native/libcp_tls_guard.so; fi
  if [[ $mode == tls ]]; then [[ $upstream =~ ^https://[0-9.]+(:[0-9]+)?/?$ && -f $native ]] || fail 'TLS 需要 HTTPS IPv4 origin 和已构建的 native 库'; fi
  if [[ -f $native ]]; then native=$(readlink -f "$native"); fi
  local has_key=false secret
  [[ -z $key_file ]] || ! $no_key || fail '--key-file 与 --no-key 不能同时使用'
  if [[ -n $key_file ]]; then
    private_file "$key_file"
    if [[ $key_file != "$config_dir/upstream.key" ]]; then install -m 0600 "$key_file" "$config_dir/upstream.key"; fi
    has_key=true
  elif ! $no_key; then
    read -r -s -p 'provider key（本机无认证上游可留空）: ' secret; printf '\n'
    if [[ -n $secret ]]; then printf '%s\n' "$secret" > "$config_dir/upstream.key"; has_key=true; fi
    unset secret
  fi
  if [[ -n $ca_file ]]; then
    [[ -f $ca_file && ! -L $ca_file ]] || fail 'CA 文件必须是普通文件'
    install -m 0600 "$ca_file" "$config_dir/upstream-ca.crt"; ca_file=$config_dir/upstream-ca.crt
  fi
  [[ -e $config_dir/policy.yaml ]] || install -m 0600 "$CP_REPO_ROOT/examples/policy.yaml" "$config_dir/policy.yaml"
  [[ -e $config_dir/gateway.token ]] || openssl rand -hex 32 > "$config_dir/gateway.token"
  private_file "$config_dir/gateway.token"
  printf 'Authorization: Bearer %s\n' "$(cat "$config_dir/gateway.token")" > "$config_dir/curl.headers"
  jq -n --arg mode "$mode" --arg upstream "$upstream" --arg model "$model" --argjson port "$port" \
    --argjson has_key "$has_key" --arg native "$native" --arg ca "$ca_file" \
    '{mode:$mode,upstream:$upstream,model:$model,port:$port,has_key:$has_key,native_library:$native,ca_file:$ca}' > "$config_dir/config.json"
  note "已准备：$config_dir（policy.yaml / 私有凭据 / config.json）；下一步 ./cpctl start"
}
op_start() {
  load_config; find_bins; need curl
  process_matches && fail '本实例已经运行'
  local args=(serve --policy "$config_dir/policy.yaml" --upstream "$upstream" --model "$model"
    --token-file "$config_dir/gateway.token" --run-state-file "$config_dir/run.json" --host 127.0.0.1 --port "$port")
  [[ $(jq -r .has_key "$config_dir/config.json") == false ]] || args+=(--upstream-key-file "$config_dir/upstream.key")
  [[ $upstream != http://* ]] || args+=(--allow-http-test-upstream)
  [[ $mode != tls ]] || args+=(--tls-direct)
  [[ -z $ca_file ]] || args+=(--upstream-ca-file "$ca_file")
  local pid ticks code previous_run=
  if [[ -f $config_dir/run.json ]]; then previous_run=$(jq -r .run_id "$config_dir/run.json"); fi
  nohup "$bin_dir/censorprivacy" "${args[@]}" 9>&- </dev/null > "$config_dir/service.log" 2>&1 &
  pid=$!
  ticks=$(awk '{print $22}' "/proc/$pid/stat")
  jq -n --argjson pid "$pid" --arg ticks "$ticks" '{pid:$pid,start_ticks:$ticks}' > "$config_dir/process.json"
  for _ in {1..80}; do
    if ! kill -0 "$pid" 2>/dev/null; then fail "服务启动失败，查看 $config_dir/service.log 中固定错误码"; fi
    code=$(auth_curl --max-time 1 --output "$config_dir/ready.json" --write-out '%{http_code}' "http://127.0.0.1:$port/ready" 2>/dev/null) || code=000
    if [[ $code == 200 ]] && process_matches && jq -e --arg previous "$previous_run" --slurpfile run "$config_dir/run.json" '.run_id == $run[0].run_id and .run_id != $previous' "$config_dir/ready.json" >/dev/null; then note "已启动：http://127.0.0.1:$port（$mode）；./cpctl check"; return; fi
    sleep 0.1
  done
  op_stop; fail '启动等待超时'
}
op_stop() {
  # Stopping an owned process must still work after a configuration edit fails.
  find_bins
  if process_matches; then
    local pid; pid=$(jq -r .pid "$config_dir/process.json")
    kill -TERM "$pid"
    for _ in {1..100}; do
      if ! process_matches; then note '已停止；旧 run 引用失效。'; return; fi
      sleep 0.05
    done
    fail '停止等待超时；请检查本实例状态'
  else note '当前实例没有匹配的受管进程，不发送信号。'; fi
}
op_check() {
  load_config; find_bins; need curl
  "$bin_dir/censorprivacy" --version
  private_file "$config_dir/gateway.token"; private_file "$config_dir/run.json"
  [[ $(stat -c %a "$config_dir") == 700 ]] || fail '配置目录应为 0700'
  auth_curl --fail "http://127.0.0.1:$port/ready" > "$config_dir/ready.json"
  jq -e --arg version "$("$bin_dir/censorprivacy" --version | awk '{print $2}')" --slurpfile run "$config_dir/run.json" '.status == "ready" and .version == $version and .run_id == $run[0].run_id' "$config_dir/ready.json" >/dev/null || fail 'run ID 或服务版本不匹配'
  local code
  code=$(curl --noproxy '*' --silent --show-error --max-time 5 --output /dev/null --write-out '%{http_code}' "http://127.0.0.1:$port/ready")
  [[ $code == 401 ]] || fail '无认证探测未返回 401'
  note '通过：安装版本、0600 凭据/状态、0700 目录、认证、run ID（未验证真实上游）'
  if $probe_model; then
    [[ $mode == gateway ]] || fail 'TLS 模式请运行 ./cpctl demo tls 或 ./cpctl dsh cloud'
    jq -n --arg model "$model" '{model:$model,messages:[{role:"user",content:"Please reply OK."}],stream:false}' > "$config_dir/probe.json"
    code=$(auth_curl --header 'Content-Type: application/json' --data-binary "@$config_dir/probe.json" --output "$config_dir/probe-response.json" --write-out '%{http_code}' "http://127.0.0.1:$port/v1/chat/completions")
    if [[ $code != 200 ]]; then jq -c '{error:.error}' "$config_dir/probe-response.json"; fail "模型请求失败 HTTP $code"; fi
    note '通过：实际模型请求（回复保存在私有 probe-response.json）'
  fi
}
op_systemd() (
  local action=${rest[0]:-install} instance unit stage state_path service
  [[ ${#rest[@]} -le 1 && $action =~ ^(install|start|stop|check)$ ]] || fail '使用 systemd install|start|stop|check'
  [[ $action == install ]] || ! $dry_run || fail '--dry-run 仅用于 systemd install'
  instance="managed-$EUID-$(printf '%s' "$config_dir" | sha256sum | cut -c1-12)"
  service=censorprivacy-$instance.service
  state_path=/var/lib/censorprivacy/$instance.run.json
  if [[ $action == stop ]]; then admin systemctl stop "$service"; note '系统实例已停止。'; return; fi
  load_config; find_bins
  case "$action" in
    start)
      if process_matches; then op_stop; fi
      admin systemctl start "$service"
      rest=(check); op_systemd; return
      ;;
    check)
      admin systemctl is-active --quiet "$service" || fail '系统实例未运行'
      [[ $(admin stat -c %a "$state_path") == 600 && $(admin stat -c %a "/etc/censorprivacy/$instance.token") == 600 ]] || fail '系统状态或凭据权限错误'
      local run_id
      run_id=$(admin jq -er .run_id "$state_path")
      auth_curl --fail --max-time 5 "http://127.0.0.1:$port/ready" > "$config_dir/system-ready.json"
      jq -e --arg run "$run_id" '.status == "ready" and .run_id == $run' "$config_dir/system-ready.json" >/dev/null || fail '系统实例 run ID 不匹配'
      [[ $(curl --noproxy '*' --silent --max-time 5 --output /dev/null --write-out '%{http_code}' "http://127.0.0.1:$port/ready") == 401 ]] || fail '认证检查失败'
      # Give this trusted control user a private copy for tls-run/DSH readiness.
      admin cat "$state_path" > "$config_dir/run.json"
      private_file "$config_dir/run.json"
      note '通过：系统服务存活、认证与 run ID；已同步本次私有 run 文件，可运行 dsh setup。'
      return
      ;;
  esac
  stage=$(mktemp -d)
  trap 'rm -rf -- "$stage"' EXIT
  [[ $bin_dir == /usr/* || $bin_dir == /opt/* ]] || fail '系统服务需要 /usr 或 /opt 下的公共安装；先 ./cpctl install --prefix /usr/local'
  [[ $bin_dir =~ ^/[a-zA-Z0-9_./-]+$ ]] || fail 'systemd 可执行文件路径含不支持的字符'
  unit=$stage/$service
  sed -e "s|%i|$instance|g" -e "s|/usr/local/bin/censorprivacy|$bin_dir/censorprivacy|g" "$CP_REPO_ROOT/deploy/censorprivacy@.service" > "$unit"
  if [[ $(jq -r .has_key "$config_dir/config.json") == false ]]; then sed -i "s| --upstream-key-file /etc/censorprivacy/$instance.upstream-key||" "$unit"; fi
  if [[ $mode == tls ]]; then sed -i '/^ExecStart=/ s|$| --tls-direct|' "$unit"; fi
  if [[ $upstream == http://* ]]; then sed -i '/^ExecStart=/ s|$| --allow-http-test-upstream|' "$unit"; fi
  if [[ -n $ca_file ]]; then sed -i "/^ExecStart=/ s|$| --upstream-ca-file /etc/censorprivacy/$instance.ca.crt|" "$unit"; fi
  printf 'CP_UPSTREAM=%s\nCP_BIND=127.0.0.1\nCP_PORT=%s\nCP_MODEL=%s\n' "$upstream" "$port" "$model" > "$stage/config.env"
  if $dry_run; then cat "$unit"; note '仅预览，未安装或启动服务。' >&2; return; fi
  if admin systemctl is-active --quiet "$service"; then fail '先 ./cpctl systemd stop，再重新安装配置'; fi
  getent group censorprivacy >/dev/null || admin groupadd --system censorprivacy
  id -u censorprivacy >/dev/null 2>&1 || admin useradd --system --gid censorprivacy --home-dir /var/lib/censorprivacy --no-create-home --shell /sbin/nologin censorprivacy
  admin install -d -o root -g censorprivacy -m 0750 /etc/censorprivacy
  admin install -d -o censorprivacy -g censorprivacy -m 0700 /var/lib/censorprivacy
  admin install -o root -g censorprivacy -m 0640 "$stage/config.env" "/etc/censorprivacy/$instance.env"
  admin install -o root -g censorprivacy -m 0640 "$config_dir/policy.yaml" "/etc/censorprivacy/$instance.policy.yaml"
  admin install -o censorprivacy -g censorprivacy -m 0600 "$config_dir/gateway.token" "/etc/censorprivacy/$instance.token"
  if [[ $(jq -r .has_key "$config_dir/config.json") == true ]]; then admin install -o censorprivacy -g censorprivacy -m 0600 "$config_dir/upstream.key" "/etc/censorprivacy/$instance.upstream-key"; fi
  if [[ -n $ca_file ]]; then admin install -o censorprivacy -g censorprivacy -m 0600 "$ca_file" "/etc/censorprivacy/$instance.ca.crt"; fi
  admin install -m 0644 "$unit" /etc/systemd/system/
  admin systemctl daemon-reload
  note "已安装 $service；下一步 ./cpctl systemd start（启动后自动检查）"
)
