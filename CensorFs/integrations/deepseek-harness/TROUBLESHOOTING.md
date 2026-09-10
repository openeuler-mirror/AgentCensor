# Troubleshooting

## Backing store must be XFS or ext4

CensorFS refuses a store whose backing filesystem is neither (e.g. `tmpfs`: `/tmp`). Symptom: `backing filesystem magic 0x… is not XFS/ext4` in `cargo test` or at `init`. Run tests/stores under an XFS/ext4 path (`export TMPDIR=/var/tmp` for `cargo test`).

## Install from the packed tarball, not a directory

`dsh plugin … add`/pnpm with a directory or `file:`/`link:` spec can leave the plugin unable to resolve peers at boot (`Cannot find package '@deepseek-ai/dsh-llm'`) or silently load a stale copy. Always `pnpm pack` first, then add the `.tgz`; when upgrading in place, `remove` + delete the installed copy + `add` the new tarball (same version+filename is otherwise reused from the pnpm store).

## Child workers need the credentials file, not an env key

The one-shot worker may boot under `sudo env_reset`, which wipes `DEEPSEEK_API_KEY`. Provide `<DSH_HOME>/.credentials.yaml` (mode 0600, flat `DEEPSEEK_API_KEY: sk-…`); the adapter deliberately reconstructs only `HOME`/`DSH_HOME`.

## Web binds 127.0.0.1 only

`--host 0.0.0.0` is refused by design. To reach the UI from another machine, SSH-tunnel: `ssh -L 3867:127.0.0.1:3867 user@host -p <port>` then open `http://127.0.0.1:3867`.

## `/dev/fuse` unavailable

Run on the openEuler/Linux target, not Windows. Check `test -c /dev/fuse`, install the FUSE userspace package, and verify the service/container passes the device through. Do not switch to a shared workspace as a production workaround.

## Stale daemon socket

Run `/censorfs-doctor --json`. If `daemon.live` is false while the socket exists, stop the daemon that owns the configured storage root, remove only the stale socket through the service's normal shutdown procedure, then restart `censorfsd`. Verify `CENSORFS_SOCKET` matches the daemon before retrying.

## Missing cgroup delegation

Use a dedicated cgroup v2 subtree with systemd `Delegate=yes`, then configure `CENSORFS_RUNNER_CGROUP_ROOT` and `CENSORFS_RUNNER_CGROUP_STATE_DIR`. cgroup v1 is unsupported for lifecycle/resource isolation; migrate to v2 or explicitly run process-level isolation.

## `bwrap` missing

Install bubblewrap from the target distribution and verify `command -v bwrap`. The doctor report must show an executable path before starting an in-process exploration.

## Child command missing

Set `DSH_CENSORFS_CHILD_COMMAND` to the installed `dsh-jsonrpc-agent` path for FUSE child mode. In-process mode does not use this command, but it still requires `node`, `censorfs`, `censorfs-mounter`, `bwrap`, `/dev/fuse`, and a live daemon.

## Doctor contract

`/censorfs-doctor` prints operator-friendly text. `/censorfs-doctor --json` prints one JSON object with `healthy` and `exitCode`; exit code `0` means all required checks pass, and `1` means not ready or fail-closed. The command never starts or changes system services.
