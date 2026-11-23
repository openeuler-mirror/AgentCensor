#!/usr/bin/env bash
# xiaoO uses its native OpenAI-compatible client, including SSE and tools.
op_xiaoo() {
  local action=${rest[0]:-} directory base binary= candidate token session launch_config route prepare_config prepare_log demo_source
  local system_prompt='You are xiaoO, a helpful agent connected through a local privacy gateway. Use the available tools to complete the user task. Preserve complete privacy references including their angle brackets; never guess their original values. Answer in plain text when reproducing private material. If a file path is supplied, use it exactly. Do not calculate hidden values with ordinary tools.'
  case "$action" in
    setup|tui|demo|web) [[ ${#rest[@]} == 1 ]] || fail "$action 不接受任务参数" ;;
    run) ;; # The task can also be read from stdin by xiaoO itself.
    *) fail '用法：./censorprivacy xiaoo setup|tui|run|demo|web [--xiaoO-path 源码目录]' ;;
  esac
  if [[ $action == demo ]]; then
    $xiaoo_tools || fail '文件读取演示需要工具；请显式添加 --enable-tools'
    demo_source=$CP_REPO_ROOT/examples/demo.txt
    [[ -f $demo_source && -r $demo_source ]] || fail "缺少演示文件：$demo_source"
  fi
  load_config
  [[ $mode == gateway ]] || fail 'xiaoO 当前接入网关模式；请使用 --mode gateway 的配置'
  base="http://127.0.0.1:$port"
  service_ready || fail "$readiness_error"
  [[ $(jq -r '.xiaoo_web // false' <<< "$ready_response") == true ]] || fail '当前网关未启用会话检测台；更新 censorprivacy 后执行 ./censorprivacy stop 与 ./censorprivacy start'
  if [[ $action == web ]]; then
    note "xiaoO 隐私检测台：$base/"
    return
  fi
  [[ $(jq -r '.xiaoo_startup_prepare // false' <<< "$ready_response") == true ]] || fail '当前网关不支持工具开关与启动预热；请更新并重启网关'
  if [[ $action == tui ]]; then [[ -t 0 && -t 1 ]] || fail 'tui 需要交互终端；脚本中请使用 xiaoo run 或 demo'; fi
  directory=$config_dir/xiaoo
  private_dir "$directory"
  safe_output "$directory/setup.json"
  safe_output "$directory/config.toml"
  private_dir "$directory/workspace"
  if [[ -n $xiaoo_bin ]]; then
    [[ $xiaoo_bin == /* && -f $xiaoo_bin && -x $xiaoo_bin ]] || fail '--xiaoo-bin 需要绝对路径的可执行文件'
    binary=$xiaoo_bin
  elif [[ -n $xiaoo_root ]]; then
    [[ $xiaoo_root == /* && -d $xiaoo_root ]] || fail '--xiaoO-path 需要绝对路径的源码目录'
    for candidate in "$xiaoo_root/target/release/xiaoo" "$xiaoo_root/target/debug/xiaoo"; do
      if [[ -f $candidate && -x $candidate ]]; then binary=$candidate; break; fi
    done
    [[ -n $binary ]] || fail 'xiaoO 尚未构建；先在其仓库执行 cargo build --locked --release -p xiaoo-endside，或使用 --xiaoo-bin'
  elif [[ -f $directory/setup.json ]]; then
    private_file "$directory/setup.json"
    binary=$(jq -er .binary "$directory/setup.json") || fail 'xiaoO setup.json 无效'
  else
    binary=$(command -v xiaoo || true)
  fi
  [[ $binary == /* && -f $binary && -x $binary ]] || fail '找不到 xiaoo；指定 --xiaoO-path 源码目录'
  binary=$(readlink -f -- "$binary")
  session=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
  [[ $session =~ ^[0-9a-f]{32}$ ]] || fail '无法创建会话标识'
  # Write the immutable per-process config first, so concurrent launches cannot
  # pick up each other's endpoint while updating the convenience config copy.
  private_dir "$directory/launches"
  launch_config=$directory/launches/$session.toml
  route=no-tools/
  $xiaoo_tools && route=
  safe_output "$launch_config"
  # JSON string escaping is also valid for these TOML basic string values.
  {
    printf '[llm]\nprovider = "openai-compatible"\n'
    printf 'model = %s\n' "$(jq -Rn --arg value "$model" '$value')"
    # xiaoO probes api_base itself first and stops on the gateway's 400 for
    # unsupported routes. A full endpoint avoids that discovery request.
    printf 'api_base = "%s/xiaoo/%s/%sv1/chat/completions"\n' "$base" "$session" "$route"
    printf 'api_key_env = "CP_XIAOO_GATEWAY_TOKEN"\nreasoning_effort = "off"\n'
    printf '\n[trace]\nstorage_backend = "noop"\n'
  } > "$launch_config"
  chmod 600 "$launch_config"
  install -m 600 "$launch_config" "$directory/config.toml"
  jq -n --arg binary "$binary" '{binary:$binary}' > "$directory/setup.json"
  chmod 600 "$directory/config.toml" "$directory/setup.json"
  if [[ $action == demo ]]; then
    safe_output "$directory/workspace/demo.txt"
    install -m 600 "$demo_source" "$directory/workspace/demo.txt"
  fi
  if [[ $action == setup ]]; then
    note "xiaoO 已就绪：$directory/config.toml"
    return
  fi
  token=$(cat "$config_dir/gateway.token")
  export CP_XIAOO_GATEWAY_TOKEN=$token
  export XIAOO_CONFIG=$launch_config
  # Prevent an inherited HTTP proxy from intercepting the local gateway.
  export NO_PROXY="127.0.0.1,localhost,::1${NO_PROXY:+,$NO_PROXY}"
  export no_proxy="127.0.0.1,localhost,::1${no_proxy:+,$no_proxy}"
  cd -- "$directory/workspace"
  if $xiaoo_tools; then
    prepare_config=$directory/launches/$session.prepare.toml
    prepare_log=$directory/launches/$session.prepare.log
    safe_output "$prepare_config"; safe_output "$prepare_log"
    sed "s|/xiaoo/$session/v1/chat/completions|/xiaoo/$session/prepare/v1/chat/completions|" "$launch_config" > "$prepare_config"
    note '正在准备 xiaoO 工具：加载实际工具目录并预热本地隐私检测；不调用云模型，不写入会话记录。'
    local prepare_started=$SECONDS
    # xiaoO omits tools on its final permitted turn. Keep two turns available;
    # the local READY response ends preparation immediately without a tool call.
    local prepare_args=(--cli run --reasoning-effort off --system "$system_prompt" --max-turns 2)
    [[ $action != demo ]] || prepare_args+=(--tools file_read)
    if ! XIAOO_CONFIG="$prepare_config" "$binary" "${prepare_args[@]}" -p 'Prepare the available tool definitions.' </dev/null > "$prepare_log" 2>&1; then
      fail "工具准备失败，未进入对话；查看 $prepare_log"
    fi
    note "xiaoO 工具准备完成（用时 $((SECONDS - prepare_started)) 秒），开始对话。"
  else
    note 'xiaoO 工具已关闭。' >&2
  fi
  case "$action" in
    tui)
      note "会话检测台：$base/"
      exec "$binary"
      ;;
    run)
      local args=(--cli run --reasoning-effort off --system "$system_prompt")
      $xiaoo_tools || args+=(--no-tools)
      if [[ ${#rest[@]} -gt 1 ]]; then args+=(-p "${rest[*]:1}"); fi
      exec "$binary" "${args[@]}"
      ;;
    demo)
      note 'xiaoO：通过 file_read 读取虚构工单，工具结果经网关脱敏，模型回复在本地恢复。'
      exec "$binary" --cli run --debug --reasoning-effort off --system "$system_prompt" --tools file_read --max-turns 4 \
        -p '请调用 file_read，参数 file_path 必须填写 demo.txt，使用这个相对路径，不要拼接其他目录。然后按文件开头的要求逐字复述整份工单。保留全部段落和完整占位符（包括尖括号），使用普通文本，不使用代码块、表格或链接，不计算金额。不要读取其他文件。' </dev/null
      ;;
  esac
}
