#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <code-fix|static-site|harness-self-evolution> <local-xfs-or-ext4-parent>" >&2
  echo "optional: DSH_DEMO_COMMAND='<command with {PROMPT} and {ROOT}>' DSH_DEMO_SNAPSHOT=/path/events.json" >&2
  exit 2
}
[[ $# -eq 2 ]] || usage
scenario=$1
parent=$2
case "$scenario" in
  code-fix) prompt='修复支付重试导致重复扣款的问题，尽量保持改动小；使用 python-code validation profile' ; profile=python-code ;;
  static-site) prompt='为这个活动页并行设计三个令人眼前一亮且风格明显不同的视觉方案；保持纯静态站点并使用 static-site validation profile' ; profile=static-site ;;
  harness-self-evolution) prompt='改进这份 Harness 配置，让 Agent 在推荐发布前更重视可验证证据；比较最小 prompt 修改、插件拆分和更强策略三种方向，使用 harness-config validation profile' ; profile=harness-config ;;
  *) usage ;;
esac

for binary in censorfs censorfsd censorfs-mounter; do
  command -v "$binary" >/dev/null || { echo "missing required CensorFS binary: $binary" >&2; exit 1; }
done
demo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root="$parent/censorfs-$scenario-$(date +%Y%m%d-%H%M%S)"
[[ ! -e "$root" ]] || { echo "demo target already exists: $root" >&2; exit 1; }
mkdir -p "$root/import" "$root/evidence"
cp -a "$demo_dir/$scenario/." "$root/import/"
export CENSORFS_STORAGE_ROOT="$root/.censorfs" CENSORFS_SOCKET="$root/control.sock"
censorfs init --import-root "$root/import" --branch main
censorfs daemon >"$root/daemon.log" 2>&1 &
daemon_pid=$!
cleanup() { kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" 2>/dev/null || true; }
trap cleanup EXIT
for _ in $(seq 1 100); do [[ -S "$CENSORFS_SOCKET" ]] && break; sleep 0.05; done
[[ -S "$CENSORFS_SOCKET" ]] || { echo "daemon did not create $CENSORFS_SOCKET" >&2; exit 1; }

if [[ -n "${DSH_DEMO_COMMAND:-}" ]]; then
  snapshot=${DSH_DEMO_SNAPSHOT:-$root/evidence/session-events.json}
  command=${DSH_DEMO_COMMAND//\{PROMPT\}/$prompt}
  command=${command//\{ROOT\}/$root}
  export CENSORFS_DEMO_ROOT="$root" CENSORFS_DEMO_SNAPSHOT="$snapshot" CENSORFS_DEMO_PROFILE="$profile"
  echo "Starting configured Harness demo; snapshot: $snapshot"
  bash -c "$command" >"$root/harness.log" 2>&1 &
  harness_pid=$!
  deadline=$((SECONDS + ${DSH_DEMO_TIMEOUT_SECONDS:-1800}))
  while (( SECONDS < deadline )); do
    if [[ -s "$snapshot" ]] && grep -q 'variant-prepared' "$snapshot"; then
      echo "PASS: $scenario produced a prepared Variant; evidence: $snapshot"
      wait "$harness_pid" || true
      exit 0
    fi
    if ! kill -0 "$harness_pid" 2>/dev/null; then
      echo "Harness command exited before prepared evidence; see $root/harness.log" >&2
      exit 1
    fi
    sleep 2
  done
  echo "Timed out waiting for prepared Variant; see $root/harness.log" >&2
  exit 1
fi

echo "Demo root: $root"
echo "CensorFS daemon PID: $daemon_pid"
echo "Profile: $profile"
echo "Set DSH_DEMO_COMMAND with {PROMPT} and {ROOT} to run Harness automatically."
echo "Otherwise enter: /explore 3 $prompt"
echo "For reproducible evidence, write Session events to: $root/evidence/session-events.json"
