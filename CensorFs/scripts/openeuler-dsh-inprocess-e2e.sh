#!/usr/bin/env bash
# =============================================================================
#  TEST-ONLY · TEST-ONLY · TEST-ONLY · TEST-ONLY · TEST-ONLY · TEST-ONLY
# =============================================================================
#
#  scripts/openeuler-dsh-inprocess-e2e.sh
#
#  E2E smoke for the DeepSeek Harness `branch_explore_inprocess` path
#  (integrations/deepseek-harness). It stands up a real CensorFS daemon,
#  opens real mount-namespace Views through `variant-open`, and drives the
#  real `runner-process.js` JSON-RPC surface that the plugin's RunnerManager
#  uses — health identity, fs.* tools, process.run (foreground, timeout,
#  bubblewrap confinement, background jobs + job.output/job.kill), candidate
#  Prepare, read-only Candidate validation, and variant Abort.
#
#  TEST-ONLY: run only on a disposable openEuler test machine. The run creates
#  test data (tickets/candidates) in whatever CensorFS store it uses, and in
#  self-contained mode it additionally starts a throwaway daemon in a mktemp
#  directory. It NEVER writes sudoers, NEVER mounts cgroup2, NEVER touches
#  systemd, kernel parameters, or credentials, and NEVER changes host
#  configuration. All of those are only *detected*; missing prerequisites
#  fail closed with a message.
#
#  Modes:
#    * Self-contained (default): creates import/store/socket under a dedicated
#      `mktemp` directory on local XFS/ext4, starts a test daemon, and the
#      EXIT trap unconditionally kills that daemon and removes the temporary
#      directory (plus the scratch dir used for logs and the runner client).
#    * Existing daemon: set CENSORFS_USE_EXISTING_DAEMON=1 plus CENSORFS_SOCKET
#      (and optionally CENSORFS_STORAGE_ROOT). The script only DETECTS the
#      socket and daemon liveness, then runs the E2E against it. Nothing is
#      created and nothing the host owns is cleaned up; only tickets opened by
#      this run are best-effort aborted on exit.
#
#  Requirements (detected, fail closed):
#    * Linux, openEuler (/etc/os-release), kernel >= 6.6, /dev/fuse
#    * Programs: jq, node, bwrap, timeout (+ setpriv when root, or passwordless
#      sudo for censorfs-mounter when not root — configured out of band)
#    * Release binaries in CENSORFS_BIN_DIR (default $repo_root/target/release):
#      censorfs and censorfs-mounter
#    * In-process profile: integrations/deepseek-harness/src/runner-process.js
#    * Self-contained mode: CENSORFS_TEST_PARENT on local XFS/ext4
#
#  Environment:
#    CENSORFS_BIN_DIR                   release binary directory
#    CENSORFS_TEST_PARENT              XFS/ext4 parent for the throwaway store (default /var/tmp)
#    CENSORFS_AGENT_UID / _GID         non-zero agent identity when running as root (default 65534)
#    CENSORFS_USE_EXISTING_DAEMON=1    use an existing daemon instead of starting one
#    CENSORFS_SOCKET                   socket to use in existing-daemon mode
#    CENSORFS_STORAGE_ROOT             store root to verify in existing-daemon mode (optional)
#    DSH_INPROCESS_E2E_UNIT_TESTS=1    additionally run `npm test` in integrations/deepseek-harness
# =============================================================================

set -euo pipefail

log() { printf '== %s\n' "$*"; }
die() { printf 'FAIL: %s\n' "$*" >&2; exit 2; }

# ---------------------------------------------------------------------------
# Fail-closed environment / OS / kernel / FUSE detection
# ---------------------------------------------------------------------------
[[ "$(uname -s)" == Linux ]] || die "Linux is required"
[[ -r /etc/os-release ]] || die "/etc/os-release is missing; cannot verify openEuler"
# shellcheck disable=SC1091
. /etc/os-release
os_identity="${ID:-} ${ID_LIKE:-} ${NAME:-}"
[[ ${os_identity,,} == *openeuler* ]] || die "this script targets openEuler (detected ${PRETTY_NAME:-unknown})"

kernel_release=$(uname -r)
kernel_major=${kernel_release%%.*}
kernel_remainder=${kernel_release#*.}
kernel_minor=${kernel_remainder%%.*}
[[ $kernel_major =~ ^[0-9]+$ && $kernel_minor =~ ^[0-9]+$ ]] || die "cannot parse kernel version: $kernel_release"
(( kernel_major > 6 || (kernel_major == 6 && kernel_minor >= 6) )) || die "CensorFS requires Linux 6.6 or newer (kernel $kernel_release)"
[[ -c /dev/fuse ]] || die "/dev/fuse is missing"

require() { command -v "$1" >/dev/null 2>&1 || die "required program not found: $1"; }
for program in jq node bwrap timeout; do require "$program"; done

# ---------------------------------------------------------------------------
# Unique RFC 4122 UUID v4 request ids for every control-plane call (the Rust
# CLI validates the request-id as a UUID). node is required above; on failure
# die() exits so set -e semantics are preserved.
# ---------------------------------------------------------------------------
request_id() {
  local id
  id=$(node -e 'process.stdout.write(require("node:crypto").randomUUID())') \
    || die "request_id: failed to generate a UUID via node"
  printf '%s\n' "$id"
}

# ---------------------------------------------------------------------------
# Repository layout: release binaries and the in-process runner profile
# ---------------------------------------------------------------------------
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${CENSORFS_BIN_DIR:-"$repo_root/target/release"}
runner_script="$repo_root/integrations/deepseek-harness/src/runner-process.js"
[[ -x "$bin/censorfs" ]] || die "missing release executable: $bin/censorfs (run scripts/build-openeuler-aarch64.sh first)"
[[ -x "$bin/censorfs-mounter" ]] || die "missing release executable: $bin/censorfs-mounter (run scripts/build-openeuler-aarch64.sh first)"
[[ -f "$runner_script" ]] || die "missing in-process runner profile: $runner_script"

# ---------------------------------------------------------------------------
# Privilege detection (never configured here — only detected, fail closed)
# ---------------------------------------------------------------------------
agent=()     # control-plane wrapper; setpriv when root + self-contained
mounter=()   # mounter argv; sudo -n prefix when non-root
uid=${CENSORFS_AGENT_UID:-65534}
gid=${CENSORFS_AGENT_GID:-65534}
if [[ $(id -u) -eq 0 ]]; then
  if [[ ${CENSORFS_USE_EXISTING_DAEMON:-0} == 1 ]]; then
    # Existing daemon: talk to it as root; the daemon decides view ownership.
    mounter=("$bin/censorfs-mounter")
  else
    [[ $uid -ne 0 && $gid -ne 0 ]] || die "root self-contained mode requires non-zero CENSORFS_AGENT_UID/GID"
    require setpriv
    agent=(setpriv --reuid "$uid" --regid "$gid" --clear-groups)
    mounter=("$bin/censorfs-mounter")
  fi
else
  require sudo
  sudo -n true 2>/dev/null || die "passwordless sudo is required (configure it out of band; this script never writes sudoers)"
  sudo -n -l "$bin/censorfs-mounter" >/dev/null 2>&1 \
    || die "passwordless sudo does not authorize $bin/censorfs-mounter (configure it out of band; this script never writes sudoers)"
  mounter=(sudo -n "$bin/censorfs-mounter")
fi

# ---------------------------------------------------------------------------
# State for the unconditional EXIT trap
# ---------------------------------------------------------------------------
owns_test_root=0
owns_daemon=0
daemon_pid=
test_root=
store=
socket=
scratch=
run_id=
declare -a e2e_tickets=() e2e_views=()

cleanup() {
  # Best-effort abort of tickets this run opened (safe in both modes; the
  # daemon tolerates aborting prepared/already-aborted variants).
  if [[ -n "${socket:-}" && ${#e2e_tickets[@]} -gt 0 ]]; then
    for index in "${!e2e_tickets[@]}"; do
      [[ -n "${e2e_tickets[$index]:-}" ]] || continue
      view=${e2e_views[$index]:-}
      args=(variant-abort --ticket "${e2e_tickets[$index]}" --run "${run_id:-e2e}" --variant "inprocess-${index}")
      if [[ -n "$view" ]]; then args+=(--view "$view"); fi
      "${agent[@]}" "$bin/censorfs" --socket "$socket" \
        --request-id "$(request_id)" "${args[@]}" >/dev/null 2>&1 || true
    done
  fi
  # Unconditional cleanup of self-created daemon and temporary directories.
  if [[ $owns_daemon -eq 1 && -n "$daemon_pid" ]]; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  if [[ $owns_test_root -eq 1 && -n "$test_root" && -d "$test_root" ]]; then
    rm -rf -- "$test_root"
  fi
  if [[ -n "$scratch" && -d "$scratch" ]]; then
    rm -rf -- "$scratch"
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Mode setup: existing daemon (detect + use only) or self-contained store
# ---------------------------------------------------------------------------
if [[ ${CENSORFS_USE_EXISTING_DAEMON:-0} == 1 ]]; then
  [[ -n "${CENSORFS_SOCKET:-}" ]] || die "CENSORFS_USE_EXISTING_DAEMON=1 requires CENSORFS_SOCKET"
  socket=$CENSORFS_SOCKET
  [[ -S "$socket" ]] || die "configured socket is not a Unix socket: $socket"
  if [[ -n "${CENSORFS_STORAGE_ROOT:-}" ]]; then
    [[ -d "$CENSORFS_STORAGE_ROOT" ]] || die "configured CENSORFS_STORAGE_ROOT is not a directory: $CENSORFS_STORAGE_ROOT"
  fi
  "${agent[@]}" "$bin/censorfs" --socket "$socket" --json head main >/dev/null \
    || die "daemon at $socket did not answer 'censorfs head main' (wrong user or daemon?)"
  export CENSORFS_SOCKET="$socket"
  log "TEST-ONLY: using existing daemon at $socket; nothing will be created or cleaned up"
else
  test_parent=${CENSORFS_TEST_PARENT:-/var/tmp}
  [[ -d "$test_parent" ]] || die "CENSORFS_TEST_PARENT is not a directory: $test_parent"
  test_root=$(mktemp -d "$test_parent/censorfs-dsh-inprocess.XXXXXX") || die "mktemp failed under $test_parent"
  owns_test_root=1
  backing=$(stat -f -c %T "$test_root")
  [[ "$backing" == xfs || "$backing" == ext2/ext3 ]] \
    || die "test store must be local XFS or ext4 (found $backing)"
  if [[ $(id -u) -eq 0 ]]; then chown "$uid:$gid" "$test_root"; fi
  mkdir -p "$test_root/import"
  printf 'initial\n' >"$test_root/import/shared.txt"
  store="$test_root/.censorfs"
  socket="$test_root/control.sock"
  export CENSORFS_STORAGE_ROOT="$store"
  export CENSORFS_SOCKET="$socket"
  "${agent[@]}" "$bin/censorfs" --storage-root "$store" --socket "$socket" init \
    --import-root "$test_root/import" --branch main >/dev/null
  "${agent[@]}" "$bin/censorfs" --storage-root "$store" --socket "$socket" daemon \
    >"$test_root/daemon.log" 2>&1 &
  daemon_pid=$!
  owns_daemon=1
  for _ in $(seq 1 100); do [[ -S "$socket" ]] && break; sleep 0.05; done
  [[ -S "$socket" ]] || die "test daemon did not create $socket (see $test_root/daemon.log)"
  "${agent[@]}" "$bin/censorfs" --socket "$socket" --json head main >/dev/null \
    || die "test daemon at $socket did not answer 'censorfs head main'"
  log "TEST-ONLY: self-contained daemon started (store=$store socket=$socket pid=$daemon_pid)"
fi

scratch=$(mktemp -d "${TMPDIR:-/tmp}/censorfs-dsh-inprocess-e2e.XXXXXX") || die "cannot create scratch directory"

# ---------------------------------------------------------------------------
# TEST-ONLY in-process Runner E2E client (Node). Reproduces RunnerManager.launch
# and drives the runner JSON-RPC surface used by branch_explore_inprocess.
# ---------------------------------------------------------------------------
cat >"$scratch/runner-e2e.mjs" <<'EOF'
#!/usr/bin/env node
// TEST-ONLY E2E client for the branch_explore_inprocess Runner mechanism.
// Mirrors RunnerManager.launch's spawn argv and readiness identity contract,
// then exercises the fs.* / process.run / job.* tool surface.
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'

const config = JSON.parse(process.argv[2])
const { socket, viewId, uid, gid, runnerId, value, runId, variantId, nodePath, runnerScript, mounter } = config

let failures = 0
function check(name, ok, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` -- ${detail}` : ''}`)
  if (!ok) failures += 1
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

class RunnerClient {
  constructor(child) {
    this.child = child
    this.pending = new Map()
    this.sequence = 0
    this.closed = false
    this.runnerId = runnerId
    this.lines = createInterface({ input: child.stdout, crlfDelay: Infinity })
    this.lines.on('line', (line) => {
      let response
      try { response = JSON.parse(line) } catch (error) { this.failAll(new Error(`invalid JSON from runner: ${error}`)); return }
      const pending = this.pending.get(response.id)
      if (pending === undefined) return
      this.pending.delete(response.id)
      if (response.ok) pending.resolve(response.result)
      else pending.reject(Object.assign(new Error(response.error?.message ?? 'runner error'), { code: response.error?.code }))
    })
    child.once('error', (error) => this.failAll(error))
  }
  call(method, params = {}) {
    if (this.closed) return Promise.reject(new Error('runner is closed'))
    const id = `${this.runnerId}:${++this.sequence}`
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
      this.child.stdin.write(`${JSON.stringify({ id, method, params })}\n`, (error) => {
        if (error === null || error === undefined) return
        this.pending.delete(id)
        reject(error)
      })
    })
  }
  failAll(error) {
    this.closed = true
    for (const pending of this.pending.values()) pending.reject(error)
    this.pending.clear()
  }
}

const argv = [
  '--socket', socket,
  '--view-id', viewId,
  '--uid', String(uid),
  '--gid', String(gid),
  '--',
  nodePath, runnerScript,
  '--runner-id', runnerId,
  '--view-id', viewId,
]
const child = spawn(mounter[0], [...mounter.slice(1), ...argv], {
  stdio: ['pipe', 'pipe', 'pipe'],
  env: { ...process.env, TMPDIR: '/tmp', CENSORFS_RUN_ID: runId, CENSORFS_VARIANT_ID: variantId, CENSORFS_RUNNER_ID: runnerId },
})
const exited = new Promise((resolve) => child.once('close', (code, signal) => resolve({ code, signal })))
child.stderr.on('data', (chunk) => process.stderr.write(`[runner-${runnerId} stderr] ${chunk.toString('utf8')}`))

const runner = new RunnerClient(child)
try {
  const health = await runner.call('health')
  check('health.protocolVersion', health.protocolVersion === 1, `got ${health.protocolVersion}`)
  check('health.runnerId', health.runnerId === runnerId, `got ${health.runnerId}`)
  check('health.viewId', health.viewId === viewId, `got ${health.viewId}`)
  check('health.cwd', health.cwd === '/workspace', `got ${health.cwd}`)

  const write = await runner.call('fs.write', { file_path: '/workspace/shared.txt', content: value })
  // /workspace/shared.txt already exists in the view (seeded from main), so a
  // write must report 'update' — 'create' would mean the wrong view was opened.
  check('fs.write.update', write.operation === 'update', write.operation)

  const read = await runner.call('fs.read', { file_path: '/workspace/shared.txt' })
  check('fs.read.value', read.lines.length === 1 && read.lines[0].text === value, JSON.stringify(read.lines))

  const note = `/workspace/note-${variantId}.txt`
  await runner.call('fs.write', { file_path: note, content: `seed-${value}\n` })
  const edit = await runner.call('fs.edit', { file_path: note, old_string: `seed-${value}`, new_string: `edited-${value}` })
  check('fs.edit.unique', edit.after === `edited-${value}\n`, JSON.stringify(edit.after))

  const glob = await runner.call('fs.glob', { pattern: '*.txt', path: '/workspace' })
  check('fs.glob.files', Array.isArray(glob.paths) && glob.paths.includes('shared.txt') && glob.paths.includes(`note-${variantId}.txt`), JSON.stringify(glob.paths))

  const grep = await runner.call('fs.grep', { pattern: value })
  check('fs.grep.match', Array.isArray(grep.matches) && grep.matches.some((match) => match.line === value), JSON.stringify(grep.matches?.slice(0, 3)))

  let readConfined = false
  try { await runner.call('fs.read', { file_path: '/etc/passwd' }) } catch (error) { readConfined = error.code === 'PATH_OUTSIDE_WORKSPACE' }
  check('fs.read.confinement', readConfined, 'runner must reject reads outside /workspace')

  let writeConfined = false
  try { await runner.call('fs.write', { file_path: '/tmp/evil.txt', content: 'x' }) } catch (error) { writeConfined = error.code === 'PATH_OUTSIDE_WORKSPACE' }
  check('fs.write.confinement', writeConfined, 'runner must reject writes outside /workspace')

  const shell = await runner.call('process.run', {
    command: 'printf -- -shell >> /workspace/shared.txt; cat /workspace/shared.txt',
    timeoutMs: 10000,
  })
  check('process.run.exitCode', shell.exitCode === 0, `exit ${shell.exitCode}`)
  // The runner keeps stdout verbatim, including any trailing newline, so
  // compare after trimming the trailing newline only.
  check('process.run.stdout', shell.stdout?.text?.trimEnd() === `${value}-shell`, `got ${JSON.stringify(shell.stdout?.text)}`)

  const sandbox = await runner.call('process.run', {
    command: 'test ! -e /home/yyy/censorfs && ! (printf forbidden >/etc/sudoers) 2>/dev/null && printf -- sandboxed',
    timeoutMs: 10000,
  })
  check('process.run.bwrap', sandbox.exitCode === 0 && sandbox.stdout?.text?.trimEnd() === 'sandboxed',
    JSON.stringify({ exitCode: sandbox.exitCode, stdout: sandbox.stdout?.text }))

  const timed = await runner.call('process.run', {
    command: '(sleep 1; touch /workspace/must-not-leak) & wait',
    timeoutMs: 100,
  })
  check('process.run.timeout', timed.timedOut === true, JSON.stringify({ timedOut: timed.timedOut, exitCode: timed.exitCode }))

  await sleep(1500)
  const leaked = await runner.call('process.run', { command: 'test ! -e /workspace/must-not-leak', timeoutMs: 10000 })
  check('process.run.killed', leaked.exitCode === 0, `exit ${leaked.exitCode}`)

  const bg = await runner.call('process.run', {
    command: 'sleep 0.6; printf bgdone >/workspace/bg.txt; printf -- bg-ok',
    run_in_background: true,
  })
  check('job.start', typeof bg.jobId === 'string' && bg.jobId.startsWith(`${runnerId}:job:`), JSON.stringify(bg))
  const bgOut = await runner.call('job.output', { job_id: bg.jobId, wait: true, timeout_ms: 15000 })
  check('job.output', bgOut.text.includes('bg-ok'), JSON.stringify(bgOut.text))
  const bgRead = await runner.call('fs.read', { file_path: '/workspace/bg.txt' })
  check('job.write', bgRead.lines.length === 1 && bgRead.lines[0].text === 'bgdone', JSON.stringify(bgRead.lines))

  const bgKill = await runner.call('process.run', { command: 'sleep 30', run_in_background: true })
  const killOut = await runner.call('job.kill', { job_id: bgKill.jobId })
  check('job.kill.requested', killOut.outcome === 'cancellation-requested', JSON.stringify(killOut))
  const killed = await runner.call('job.output', { job_id: bgKill.jobId, wait: true, timeout_ms: 15000 })
  check('job.kill.settled', killed.job?.status === 'killed', JSON.stringify(killed.job?.status))

  const shutdown = await runner.call('shutdown')
  check('runner.shutdown', shutdown.shutdown === true, JSON.stringify(shutdown))
} catch (error) {
  check(`unexpected client error: ${error.message}`, false, error.code ?? '')
} finally {
  const { code, signal } = await exited
  check('runner.exit', code === 0, `code ${code} signal ${signal}`)
}

process.exit(failures === 0 ? 0 : 1)
EOF

# ---------------------------------------------------------------------------
# E2E body: two in-process Runners on private Views, then Prepare / validate
# ---------------------------------------------------------------------------
run_id="dsh-inprocess-e2e-$(date +%s)"
head_json=$("${agent[@]}" "$bin/censorfs" --socket "$socket" --json head main)
generation=$(jq -r .generation_id <<<"$head_json")
head_seq=$(jq -r .head_seq <<<"$head_json")
[[ -n "$generation" && "$head_seq" =~ ^[0-9]+$ ]] || die "could not read main head from daemon"

values=(alpha beta)
variant_ids=(inprocess-a inprocess-b)
for index in "${!variant_ids[@]}"; do
  variant=${variant_ids[$index]}
  value=${values[$index]}
  opened=$("${agent[@]}" "$bin/censorfs" --socket "$socket" \
    --request-id "$(request_id)" variant-open \
    --branch main --expected-generation "$generation" --expected-head-seq "$head_seq" \
    --run "$run_id" --variant "$variant")
  ticket=$(jq -r .result.ticket.ticket_id <<<"$opened")
  view=$(jq -r .result.view.view_id <<<"$opened")
  view_uid=$(jq -r .result.view.owner_uid <<<"$opened")
  view_gid=$(jq -r .result.view.owner_gid <<<"$opened")
  [[ -n "$ticket" && -n "$view" && "$view_uid" =~ ^[0-9]+$ && "$view_gid" =~ ^[0-9]+$ ]] \
    || die "variant-open returned an incomplete result for $variant"
  e2e_tickets[$index]=$ticket
  e2e_views[$index]=$view

  runner_id="runner-$index"
  mounter_json=$(printf '%s\n' "${mounter[@]}" | jq -R . | jq -s -c .)
  cfg=$(jq -nc \
    --arg socket "$socket" --arg viewId "$view" --arg uid "$view_uid" --arg gid "$view_gid" \
    --arg runnerId "$runner_id" --arg value "$value" --arg runId "$run_id" --arg variantId "$variant" \
    --arg nodePath "$(command -v node)" --arg runnerScript "$runner_script" \
    --argjson mounter "$mounter_json" \
    '{socket:$socket, viewId:$viewId, uid:$uid, gid:$gid, runnerId:$runnerId, value:$value, runId:$runId, variantId:$variantId, nodePath:$nodePath, runnerScript:$runnerScript, mounter:$mounter}')
  log "TEST-ONLY: driving in-process Runner for $variant (view $view, ticket $ticket)"
  if ! timeout 180 "$(command -v node)" "$scratch/runner-e2e.mjs" "$cfg" >"$scratch/runner-$index.log" 2>&1; then
    cat "$scratch/runner-$index.log" >&2
    die "runner E2E failed for $variant"
  fi
  cat "$scratch/runner-$index.log"
done

log "TEST-ONLY: verifying stable branch isolation"
stable=$("${agent[@]}" "$bin/censorfs" --socket "$socket" cat --branch main /shared.txt)
[[ "$stable" == "initial" ]] || die "stable main branch was mutated by in-process runners (got: $stable)"

declare -a candidates=()
for index in "${!variant_ids[@]}"; do
  variant=${variant_ids[$index]}
  prepared=$("${agent[@]}" "$bin/censorfs" --socket "$socket" \
    --request-id "$(request_id)" variant-prepare \
    --ticket "${e2e_tickets[$index]}" --view "${e2e_views[$index]}" \
    --run "$run_id" --variant "$variant" \
    --timeout-ms 30000 --max-diff-file-bytes 262144)
  candidate=$(jq -r .result.candidate.candidate_id <<<"$prepared")
  [[ -n "$candidate" ]] || die "variant-prepare returned no candidate for $variant"
  candidates[$index]=$candidate
done
[[ "${candidates[0]}" != "${candidates[1]}" ]] || die "prepared candidates are not distinct"
log "TEST-ONLY: prepared candidates ${candidates[0]} and ${candidates[1]}"

for index in "${!variant_ids[@]}"; do
  variant=${variant_ids[$index]}
  value=${values[$index]}
  candidate=${candidates[$index]}
  candidate_view=$("${agent[@]}" "$bin/censorfs" --socket "$socket" \
    --request-id "$(request_id)" candidate-view-open --candidate "$candidate")
  cview=$(jq -r .result.view_id <<<"$candidate_view")
  cview_uid=$(jq -r .result.owner_uid <<<"$candidate_view")
  cview_gid=$(jq -r .result.owner_gid <<<"$candidate_view")
  [[ -n "$cview" && "$cview_uid" =~ ^[0-9]+$ && "$cview_gid" =~ ^[0-9]+$ ]] \
    || die "candidate-view-open returned no usable view for $variant"
  timeout 30 "${mounter[@]}" \
    --socket "$socket" --view-id "$cview" --uid "$cview_uid" --gid "$cview_gid" --read-only -- \
    /bin/sh -c 'test "$(cat /workspace/shared.txt)" = "$1"; ! (printf forbidden >/workspace/must-fail.txt) 2>/dev/null' \
    sh "${value}-shell" \
    || die "read-only validation failed for $variant ($candidate)"
  "${agent[@]}" "$bin/censorfs" --socket "$socket" \
    --request-id "$(request_id)" view-close --view "$cview" >/dev/null
  log "TEST-ONLY: read-only validation passed for $variant ($candidate)"
done

for index in "${!variant_ids[@]}"; do
  variant=${variant_ids[$index]}
  "${agent[@]}" "$bin/censorfs" --socket "$socket" \
    --request-id "$(request_id)" variant-abort \
    --ticket "${e2e_tickets[$index]}" --view "${e2e_views[$index]}" \
    --run "$run_id" --variant "$variant" >/dev/null
  log "TEST-ONLY: aborted $variant"
done

if [[ ${DSH_INPROCESS_E2E_UNIT_TESTS:-0} == 1 ]]; then
  integration_dir="$repo_root/integrations/deepseek-harness"
  [[ -d "$integration_dir/node_modules" ]] \
    || die "DSH_INPROCESS_E2E_UNIT_TESTS=1 requires $integration_dir/node_modules (run npm install there first)"
  log "TEST-ONLY: running deepseek-harness unit tests (npm test)"
  (cd "$integration_dir" && npm test) || die "integration unit tests failed"
fi

echo
echo "TEST-ONLY PASSED: branch_explore_inprocess E2E (Runner protocol, View isolation,"
echo "Prepare, read-only validation, abort) succeeded."
if [[ ${CENSORFS_USE_EXISTING_DAEMON:-0} == 1 ]]; then
  echo
  echo "The existing daemon was left untouched. For the model-driven leg, start the web"
  echo "profile with these variables and run /explore in chat:"
  echo "  export CENSORFS_SOCKET=$socket"
  echo "  export CENSORFS_COMMAND=$bin/censorfs"
  echo "  export CENSORFS_MOUNTER=$bin/censorfs-mounter"
else
  echo
  echo "The throwaway daemon and temporary store were removed by the EXIT trap."
  echo "For the model-driven leg against a persistent daemon, rerun with"
  echo "CENSORFS_USE_EXISTING_DAEMON=1 and CENSORFS_SOCKET=<socket>."
fi
