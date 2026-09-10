# Changelog

## Unreleased

- Added repository CI for Harness tests, JavaScript syntax checks, `cargo fmt --check`, locked workspace tests, and optional openEuler AArch64 smoke jobs.
- Added tag-aligned release automation for the two npm packages and CensorFS binary archives.
- Added machine-readable `/censorfs-doctor --json` output and persisted Runner isolation audit records.
- `childCommand` is now optional for `inProcessOnly` deployments. Existing configurations remain compatible; external `branch_explore` still requires it. Set `DSH_CENSORFS_CHILD_COMMAND` when using the FUSE child path.
- `runnerCgroup` remains accepted as a compatibility alias for `runnerIsolation`; new deployments should use `runnerIsolation`.
- MVP still has no online Candidate GC. Monitor aborted Candidate/object storage growth and cgroup residue with `/censorfs-doctor` and the platform's disk/cgroup monitoring.

Release tags must use the same `vX.Y.Z` version as both npm package manifests. Before publishing, complete `docs/RELEASE_ACCEPTANCE.md` on openEuler 24.03 AArch64 and retain smoke/model Session evidence.
