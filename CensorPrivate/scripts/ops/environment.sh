#!/usr/bin/env bash
doctor() {
  local missing=0 command version
  note "系统：$(uname -srm)；配置目录：$config_dir"
  [[ $(uname -s) == Linux ]] || { note '缺失：当前工具面向 Linux'; missing=1; }
  local commands=(bash curl jq openssl install stat flock)
  if [[ -f $CP_REPO_ROOT/Cargo.toml ]]; then commands+=(cc make pkg-config cargo rustc); fi
  if $demo || $dsh; then commands+=(node); fi
  if $runtime; then commands+=(ip nft unshare); fi
  for command in "${commands[@]}"; do
    if command -v "$command" >/dev/null; then note "OK $command"; else note "缺失：$command"; missing=1; fi
  done
  if [[ -f $CP_REPO_ROOT/Cargo.toml ]] && command -v rustc >/dev/null; then
    version=$(rustc --version | awk '{print $2}')
    if [[ $(printf '%s\n' 1.85.0 "$version" | sort -V | head -n 1) != 1.85.0 ]]; then note '需要 Rust >= 1.85'; missing=1; fi
  fi
  if $demo || $dsh; then
    if command -v node >/dev/null && ! node -e 'let [a,b]=process.versions.node.split(".").map(Number);process.exit(a===22&&b>=19||a>=24?0:1)'; then note '需要 Node ^22.19 或 >=24'; missing=1; fi
  fi
  if $tls; then
    if [[ ! -f $CP_REPO_ROOT/Cargo.toml ]] || pkg-config --exists openssl 2>/dev/null; then note 'OK OpenSSL'; else note '缺失：OpenSSL 开发库'; missing=1; fi
    if [[ -x /usr/bin/curl ]] && /usr/bin/curl --version | head -n 1 | grep -q OpenSSL; then note 'OK /usr/bin/curl 动态 OpenSSL 候选（演示还会实际验证）';
    else note '缺失：TLS 演示需要 OpenSSL curl'; missing=1; fi
  fi
  if $dsh; then
    if [[ -n $dsh_root && -f $dsh_root/apps/cli/lib/bin.js ]]; then note 'OK DSH 已构建源码';
    elif command -v dsh >/dev/null; then note 'OK DSH CLI'; else note '缺失：DSH CLI 或 --dsh-root 已构建目录'; missing=1; fi
  fi
  note "磁盘剩余：$(df -h "$CP_REPO_ROOT" | awk 'NR==2 {print $4}')（首次 Rust 构建建议数 GB）"
  (( missing == 0 )) || fail '检查未通过；./cpctl deps 可安装系统包，Rust/DSH 按手册准备'
}
deps() {
  local family
  family=$(awk -F= '$1=="ID" {gsub(/"/,"",$2);print $2}' /etc/os-release)
  local packages
  case "$family" in
    ubuntu|debian)
      packages=(build-essential pkg-config ca-certificates curl jq openssl util-linux)
      $tls && packages+=(libssl-dev)
      $runtime && packages+=(iproute2 nftables util-linux libseccomp2)
      if $demo || $dsh; then packages+=(nodejs npm); fi
      execute admin apt-get update
      execute admin apt-get install -y "${packages[@]}"
      ;;
    openEuler|openeuler|rhel|centos|rocky|almalinux|fedora)
      packages=(gcc make pkgconf-pkg-config ca-certificates curl jq openssl util-linux)
      $tls && packages+=(openssl-devel)
      $runtime && packages+=(iproute nftables util-linux libseccomp)
      if $demo || $dsh; then packages+=(nodejs npm); fi
      execute admin dnf install -y "${packages[@]}"
      ;;
    *) fail "不支持自动安装此发行版：$family；按手册安装依赖" ;;
  esac
  if $dry_run; then note '仅预览，未安装依赖。'; else note '系统依赖安装完成；请重新运行 doctor 验证版本。'; fi
  note '若发行版 Node 太旧，请安装 Node ^22.19 或 >=24。'
  note 'Rust 需要 >=1.85；本工具不以 root 下载执行 rustup 安装脚本。'
}
