# Security Boundaries

- Keep Harness sessions, credentials, model logs, and caches outside the CensorFS `/workspace`; workers receive only the files in their assigned View.
- `childEnv` is an explicit allowlist. Never replace it with `process.env`, and do not place API keys in argv, prompts, Session events, or audit records.
- Runner audit records contain identifiers, isolation level, and lifecycle state only; they must be stored in a `0700` directory outside `/workspace`.
- The doctor and smoke tools are read-only with respect to production services. They do not upload or disclose sudoers, systemd unit contents, cgroup configuration, credentials, or daemon logs.
- Test smoke scripts must use isolated temporary storage, sockets, and delegated cgroup roots. Do not point them at production state.
- cgroup v1 is not treated as equivalent to v2. Use process isolation or migrate the host before enabling lifecycle/resource controls.
