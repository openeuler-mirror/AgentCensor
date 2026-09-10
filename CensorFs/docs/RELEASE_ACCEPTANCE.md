# CensorFS Release Acceptance

This checklist is a release gate for `@censorfs/deepseek-harness` and `@censorfs/event-exporter`. It must be completed on an openEuler 24.03 AArch64 host; Windows results do not satisfy these gates.

## Required evidence

- [ ] `bash scripts/openeuler-namespace-runner-smoke.sh` passes.
- [ ] `bash scripts/openeuler-namespace-runner-cgroup-fault-smoke.sh` passes.
- [ ] `bash scripts/openeuler-dsh-inprocess-e2e.sh` passes.
- [ ] `scripts/openeuler-dsh-inprocess-real-e2e.sh` passes with the configured real DeepSeek model and a saved Session event JSON snapshot.
- [ ] Snapshot proves `exploration-started` with `mode: in-process`, 2–4 `variant-running` events, `ranking-ready`, `exploration-ended`, and `variant-published`.
- [ ] `npm pack --dry-run` for both packages contains no `test/`, `demos/`, `docs/`, or Rust artifacts.

## Record

- Target: `openEuler 24.03 AArch64`
- Kernel: `<fill in>`
- CensorFS release: `<fill in>`
- Harness peer release: `0.1.0-rc.8`
- Model/provider: `<fill in>`
- Date/commit: `<fill in>`
- Evidence directory: `<fill in>`
- Result: `NOT RUN — fill this section before publishing`

## cgroup policy

Only cgroup v2 is supported for lifecycle/resource isolation. A cgroup v1 host must remain in process-only mode or be migrated before release validation; it must not be reported as passing the cgroup gate. The cgroup fault smoke must use an isolated delegated test root and must not modify production systemd or cgroup configuration.
