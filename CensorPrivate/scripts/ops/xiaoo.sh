#!/usr/bin/env bash
# xiaoO uses its native OpenAI-compatible client, including SSE and tools.
op_xiaoo() {
  local action=${rest[0]:-} directory base binary= candidate token
  local system_prompt='You are xiaoO, a helpful agent connected through a local privacy gateway. Use the available tools to complete the user task. Preserve complete privacy references including their angle brackets; never guess their original values. Answer in plain text when reproducing private material. If a file path is supplied, use it exactly. Do not calculate hidden values with ordinary tools.'
  case "$action" in
    setup|tui|demo) [[ ${#rest[@]} == 1 ]] || fail "$action 不接受任务参数" ;;
    run) ;; # The task can also be read from stdin by xiaoO itself.
    *) fail '用法：./cpctl xiaoo setup|tui|run|demo [--xiaoo-root 路径 | --xiaoo-bin 文件]' ;;
  esac
  load_config
  [[ $mode == gateway ]] || fail 'xiaoO 当前接入网关模式；请使用 --mode gateway 的配置'
  base="http://127.0.0.1:$port"
  service_ready || fail "$readiness_error"
  directory=$config_dir/xiaoo
  private_dir "$directory"
  safe_output "$directory/setup.json"
  safe_output "$directory/config.toml"
  private_dir "$directory/workspace"
  safe_output "$directory/workspace/privacy-business.txt"
  if [[ -n $xiaoo_bin ]]; then
    [[ $xiaoo_bin == /* && -f $xiaoo_bin && -x $xiaoo_bin ]] || fail '--xiaoo-bin 需要绝对路径的可执行文件'
    binary=$xiaoo_bin
  elif [[ -n $xiaoo_root ]]; then
    [[ $xiaoo_root == /* && -d $xiaoo_root ]] || fail '--xiaoo-root 需要绝对路径的源码目录'
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
  [[ $binary == /* && -f $binary && -x $binary ]] || fail '找不到 xiaoo；指定 --xiaoo-root 或 --xiaoo-bin'
  binary=$(readlink -f -- "$binary")
  # JSON string escaping is also valid for these TOML basic string values.
  {
    printf '[llm]\nprovider = "openai-compatible"\n'
    printf 'model = %s\n' "$(jq -Rn --arg value "$model" '$value')"
    # xiaoO probes api_base itself first and stops on the gateway's 400 for
    # unsupported routes. A full endpoint avoids that discovery request.
    printf 'api_base = "%s/v1/chat/completions"\n' "$base"
    printf 'api_key_env = "CP_XIAOO_GATEWAY_TOKEN"\nreasoning_effort = "off"\n'
    printf '\n[trace]\nstorage_backend = "noop"\n'
  } > "$directory/config.toml"
  jq -n --arg binary "$binary" '{binary:$binary}' > "$directory/setup.json"
  chmod 600 "$directory/config.toml" "$directory/setup.json"
  install -m 600 "$CP_REPO_ROOT/examples/privacy-business.txt" "$directory/workspace/privacy-business.txt"
  if [[ $action == setup ]]; then
    note "xiaoO 已就绪：$directory/config.toml；./cpctl xiaoo tui 或 ./cpctl xiaoo demo"
    return
  fi
  token=$(cat "$config_dir/gateway.token")
  export CP_XIAOO_GATEWAY_TOKEN=$token
  export XIAOO_CONFIG=$directory/config.toml
  # Prevent an inherited HTTP proxy from intercepting the local gateway.
  export NO_PROXY="127.0.0.1,localhost,::1${NO_PROXY:+,$NO_PROXY}"
  export no_proxy="127.0.0.1,localhost,::1${no_proxy:+,$no_proxy}"
  cd -- "$directory/workspace"
  case "$action" in
    tui)
      [[ -t 0 && -t 1 ]] || fail 'tui 需要交互终端；脚本中请使用 xiaoo run 或 demo'
      note 'xiaoO TUI：复用当前网关；可粘贴说明中的邮件。退出 xiaoO 后网关继续运行。'
      exec "$binary"
      ;;
    run)
      local args=(--cli run --reasoning-effort off --system "$system_prompt")
      if [[ ${#rest[@]} -gt 1 ]]; then args+=(-p "${rest[*]:1}"); fi
      exec "$binary" "${args[@]}"
      ;;
    demo)
      note 'xiaoO：通过 file_read 读取虚构邮件，工具结果经网关脱敏，模型回复在本地恢复。'
      exec "$binary" --cli run --debug --reasoning-effort off --system "$system_prompt" --tools file_read --max-turns 4 \
        -p '请调用 file_read，参数 file_path 必须填写 privacy-business.txt，使用这个相对路径，不要拼接其他目录。然后按文件开头的要求逐字复述整封邮件。保留全部段落和完整占位符（包括尖括号），使用普通文本，不使用代码块、表格或链接，不计算金额。不要读取其他文件。' </dev/null
      ;;
  esac
}
