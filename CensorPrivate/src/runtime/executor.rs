//! Runtime lifecycle, process supervision and conservative resource cleanup.
use super::*;

pub struct LinuxRuntime<R = SystemRunner> {
    pub(super) runner: R,
}
impl Default for LinuxRuntime {
    fn default() -> Self {
        Self {
            runner: SystemRunner,
        }
    }
}
impl<R: RuntimeRunner> LinuxRuntime<R> {
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }
    async fn execute(&self, command: &RuntimeCommand) -> Result<Vec<u8>> {
        let result = self.runner.execute(command).await?;
        if !result.success {
            return Err(Error::RuntimeBlocked);
        }
        Ok(result.stdout)
    }
    async fn preflight(&self, plan: &RuntimePlan) -> Result<()> {
        if sys::effective_uid() != 0 || !matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
            return Err(Error::RuntimeBlocked);
        }
        for tool in ["ip", "nft", "unshare"] {
            trusted_tool(tool)?;
        }
        let help = self
            .execute(&RuntimeCommand::new(&["unshare", "--help"]))
            .await?;
        if !help.windows(12).any(|v| v == b"--kill-child")
            || !help.windows(12).any(|v| v == b"--mount-proc")
        {
            return Err(Error::RuntimeBlocked);
        }
        if !plan.spec.workspace.is_dir()
            || fs::canonicalize(&plan.spec.workspace).map_err(|_| Error::RuntimeBlocked)?
                != plan.spec.workspace
        {
            return Err(Error::RuntimeBlocked);
        }
        scan(&plan.spec.workspace, true, &mut 100_000)?;
        let (paths, _) = readonly_layout(&plan.spec)?;
        let mut remaining = 1_000_000;
        for path in paths {
            scan(&path, false, &mut remaining)?;
        }
        sys::check_stdio(&plan.spec.workspace).map_err(|_| Error::RuntimeBlocked)?;
        let namespaces = self
            .execute(&RuntimeCommand::new(&["ip", "netns", "list"]))
            .await?;
        if std::str::from_utf8(&namespaces)
            .map_err(|_| Error::RuntimeBlocked)?
            .lines()
            .any(|line| line.split_whitespace().next() == Some(&plan.namespace))
        {
            return Err(Error::RuntimeBlocked);
        }
        let links = json::from_slice(
            &self
                .execute(&RuntimeCommand::new(&["ip", "-j", "link", "show"]))
                .await?,
        )?;
        if links
            .as_array()
            .ok_or(Error::RuntimeBlocked)?
            .iter()
            .any(|item| {
                item.get("ifname")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == plan.host_interface || name == plan.child_interface)
            })
        {
            return Err(Error::RuntimeBlocked);
        }
        let tables = json::from_slice(
            &self
                .execute(&RuntimeCommand::new(&["nft", "-j", "list", "tables"]))
                .await?,
        )?;
        if tables
            .get("nftables")
            .and_then(Value::as_array)
            .ok_or(Error::RuntimeBlocked)?
            .iter()
            .any(|item| {
                item["table"]["family"] == "inet" && item["table"]["name"] == plan.nft_table
            })
        {
            return Err(Error::RuntimeBlocked);
        }
        let routes = json::from_slice(
            &self
                .execute(&RuntimeCommand::new(&[
                    "ip", "-j", "route", "show", "table", "all",
                ]))
                .await?,
        )?;
        let addresses = json::from_slice(
            &self
                .execute(&RuntimeCommand::new(&["ip", "-j", "addr", "show"]))
                .await?,
        )?;
        let mut prefixes = Vec::new();
        for route in routes.as_array().ok_or(Error::RuntimeBlocked)? {
            if let Some(dst) = route
                .get("dst")
                .and_then(Value::as_str)
                .filter(|dst| *dst != "default")
            {
                prefixes.push(dst.to_owned());
            }
        }
        for interface in addresses.as_array().ok_or(Error::RuntimeBlocked)? {
            for address in interface
                .get("addr_info")
                .and_then(Value::as_array)
                .ok_or(Error::RuntimeBlocked)?
            {
                if address["family"] == "inet" {
                    prefixes.push(format!(
                        "{}/{}",
                        address["local"].as_str().ok_or(Error::RuntimeBlocked)?,
                        address["prefixlen"].as_u64().ok_or(Error::RuntimeBlocked)?
                    ));
                }
            }
        }
        for prefix in prefixes {
            let network: ipnet::IpNet = prefix
                .parse()
                .or_else(|_| prefix.parse::<std::net::IpAddr>().map(ipnet::IpNet::from))
                .map_err(|_| Error::RuntimeBlocked)?;
            if let ipnet::IpNet::V4(network) = network {
                if network.prefix_len() > 0
                    && (network.contains(&plan.subnet.network())
                        || plan.subnet.contains(&network.network()))
                {
                    return Err(Error::RuntimeBlocked);
                }
            }
        }
        if let Some(guard) = &plan.spec.guard_launcher {
            let metadata = guard.metadata().map_err(|_| Error::RuntimeBlocked)?;
            if !metadata.is_file()
                || metadata.uid() != 0
                || metadata.mode() & 0o022 != 0
                || metadata.mode() & 0o111 == 0
            {
                return Err(Error::RuntimeBlocked);
            }
        }
        Ok(())
    }
    pub(super) async fn prepare(
        &self,
        plan: &RuntimePlan,
        resources: &mut Resources,
    ) -> Result<()> {
        for (index, command) in plan.setup.iter().enumerate() {
            self.execute(command).await?;
            match index {
                0 => {
                    resources.namespace = true;
                    resources.identity = Some(self.runner.namespace_identity(&plan.namespace)?);
                }
                1 => resources.link = true,
                8 => resources.table = true,
                _ => (),
            }
        }
        Ok(())
    }
    pub(super) async fn cleanup(
        &self,
        plan: &RuntimePlan,
        resources: &mut Resources,
    ) -> Result<()> {
        let result = async {
            if resources.namespace {
                let expected = resources.identity.ok_or(Error::RuntimeCleanupFailed)?;
                if self.runner.namespace_identity(&plan.namespace)? != expected {
                    return Err(Error::RuntimeCleanupFailed);
                }
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let output = self
                        .execute(&RuntimeCommand::new(&[
                            "ip",
                            "netns",
                            "pids",
                            &plan.namespace,
                        ]))
                        .await?;
                    let pids =
                        std::str::from_utf8(&output).map_err(|_| Error::RuntimeCleanupFailed)?;
                    if pids.trim().is_empty() {
                        break;
                    }
                    for pid in pids.split_whitespace() {
                        let pid: u32 = pid.parse().map_err(|_| Error::RuntimeCleanupFailed)?;
                        let fd = match sys::pidfd_open(pid) {
                            Ok(fd) => fd,
                            Err(error)
                                if matches!(
                                    error.raw_os_error(),
                                    Some(libc::ESRCH | libc::ENOENT)
                                ) =>
                            {
                                continue
                            }
                            Err(_) => return Err(Error::RuntimeCleanupFailed),
                        };
                        match identity(format!("/proc/{pid}/ns/net")) {
                            Ok(actual) if actual == expected => {
                                if let Err(error) = sys::kill_pidfd(&fd) {
                                    if error.raw_os_error() != Some(libc::ESRCH) {
                                        return Err(Error::RuntimeCleanupFailed);
                                    }
                                }
                            }
                            Ok(_) => (),
                            // A dying/zombie process can retain /proc/PID while
                            // its namespace links have already disappeared.
                            // Confirm exit using the opened identity, not PID.
                            Err(_) if sys::pidfd_exited(&fd).unwrap_or(false) => (),
                            Err(_) => return Err(Error::RuntimeCleanupFailed),
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::RuntimeCleanupFailed);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                self.execute(&RuntimeCommand::new(&[
                    "ip",
                    "netns",
                    "delete",
                    &plan.namespace,
                ]))
                .await?;
                resources.namespace = false;
            }
            if resources.link {
                let links = json::from_slice(
                    &self
                        .execute(&RuntimeCommand::new(&["ip", "-j", "link", "show"]))
                        .await?,
                )?;
                if links
                    .as_array()
                    .ok_or(Error::RuntimeCleanupFailed)?
                    .iter()
                    .any(|item| item["ifname"] == plan.host_interface)
                {
                    let deleted = self
                        .execute(&RuntimeCommand::new(&[
                            "ip",
                            "link",
                            "delete",
                            &plan.host_interface,
                        ]))
                        .await;
                    // Namespace teardown can concurrently destroy its veth.
                    // A failed delete is benign only after confirming absence.
                    if deleted.is_err() {
                        let after = json::from_slice(
                            &self
                                .execute(&RuntimeCommand::new(&["ip", "-j", "link", "show"]))
                                .await?,
                        )?;
                        if after
                            .as_array()
                            .ok_or(Error::RuntimeCleanupFailed)?
                            .iter()
                            .any(|item| item["ifname"] == plan.host_interface)
                        {
                            return Err(Error::RuntimeCleanupFailed);
                        }
                    }
                }
                resources.link = false;
            }
            if resources.table {
                self.execute(&RuntimeCommand::new(&[
                    "nft",
                    "delete",
                    "table",
                    "inet",
                    &plan.nft_table,
                ]))
                .await?;
                resources.table = false;
            }
            Ok(())
        }
        .await;
        result.map_err(|_| Error::RuntimeCleanupFailed)
    }
    fn spawn(
        &self,
        plan: &RuntimePlan,
        bootstrap_executable: &Path,
        startup_timeout: Duration,
    ) -> Result<(Child, OwnedFd)> {
        let (paths, aliases) = readonly_layout(&plan.spec)?;
        let mut environment = BTreeMap::from([
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "HOME".into(),
                plan.spec.workspace.to_string_lossy().into_owned(),
            ),
        ]);
        environment.extend(plan.spec.environment.clone());
        for (key, value) in [
            ("OPENAI_BASE_URL", plan.gateway_url() + "/v1"),
            ("ANTHROPIC_BASE_URL", plan.gateway_url()),
            ("CENSORPRIVACY_GATEWAY", plan.gateway_url()),
            ("CENSORPRIVACY_URL", plan.gateway_url()),
            ("CENSORPRIVACY_GATEWAY_IP", plan.gateway_address.to_string()),
            ("CENSORPRIVACY_RUN_ID", plan.spec.run_id.clone()),
        ] {
            environment.entry(key.into()).or_insert(value);
        }
        let read_only_identities = paths
            .iter()
            .map(|path| Ok((path.clone(), identity(path)?)))
            .collect::<Result<_>>()?;
        let configuration = bootstrap::Configuration {
            command: plan.spec.command.clone(),
            workspace: plan.spec.workspace.clone(),
            uid: plan.spec.uid,
            gid: plan.spec.gid,
            environment,
            read_only_paths: paths,
            system_aliases: aliases,
            host_mount: identity("/proc/self/ns/mnt")?,
            host_ipc: identity("/proc/self/ns/ipc")?,
            host_net: identity("/proc/self/ns/net")?,
            workspace_identity: identity(&plan.spec.workspace)?,
            read_only_identities,
        };
        let bytes =
            Zeroizing::new(serde_json::to_vec(&configuration).map_err(|_| Error::RuntimeBlocked)?);
        let config = sys::sealed_configuration(&bytes).map_err(|_| Error::RuntimeBlocked)?;
        let (reader, writer) = sys::readiness_pipe().map_err(|_| Error::RuntimeBlocked)?;
        let ip = trusted_tool("ip")?;
        let unshare = trusted_tool("unshare")?;
        let mut argv = vec![
            ip.to_string_lossy().into_owned(),
            "netns".into(),
            "exec".into(),
            plan.namespace.clone(),
            unshare.to_string_lossy().into_owned(),
            "--mount".into(),
            "--ipc".into(),
            "--pid".into(),
            "--fork".into(),
            "--kill-child=SIGKILL".into(),
            "--mount-proc".into(),
            "--propagation".into(),
            "private".into(),
            "--".into(),
            bootstrap_executable.to_string_lossy().into_owned(),
            "--internal-bootstrap".into(),
            config.as_raw_fd().to_string(),
            writer.as_raw_fd().to_string(),
        ];
        if let Some(guard) = &plan.spec.guard_launcher {
            let mut wrapped = vec![
                guard.to_string_lossy().into_owned(),
                "--scope".into(),
                plan.namespace.clone(),
                "--group".into(),
                plan.spec.guard_group.clone(),
                "--failure-policy".into(),
                "deny".into(),
                "--".into(),
            ];
            wrapped.append(&mut argv);
            argv = wrapped;
        }
        let mut command = std::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env_clear()
            .env("PATH", TOOL_PATH)
            .env("LANG", "C.UTF-8");
        let mut child = sys::spawn_with_configuration(&mut command, &config, &writer)
            .map_err(|_| Error::RuntimeBlocked)?;
        drop(writer);
        let pidfd = match sys::pidfd_open(child.id()) {
            Ok(fd) => fd,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::RuntimeBlocked);
            }
        };
        if sys::wait_ready(reader, startup_timeout).is_err() {
            let _ = sys::kill_pidfd(&pidfd);
            let _ = child.wait();
            return Err(Error::RuntimeBlocked);
        }
        Ok((child, pidfd))
    }
    pub async fn run<F, Fut>(
        &self,
        spec: RuntimeSpec,
        bootstrap_executable: &Path,
        ready_check: F,
        timeout: Option<Duration>,
        startup_timeout: Duration,
    ) -> Result<i32>
    where
        F: FnOnce(&RuntimePlan) -> Fut,
        Fut: Future<Output = Result<bool>>,
    {
        if startup_timeout.is_zero() || timeout.is_some_and(|value| value.is_zero()) {
            return Err(Error::RuntimeBlocked);
        }
        let plan = RuntimePlan::new(spec)?;
        self.preflight(&plan).await?;
        let mut resources = Resources::default();
        let result = async {
            self.prepare(&plan,&mut resources).await?;
            if !ready_check(&plan).await? { return Err(Error::RuntimeBlocked); }
            let (process,pidfd) = self.spawn(&plan,bootstrap_executable,startup_timeout)?;
            let mut child=RuntimeChild {process,pidfd};
            let started = Instant::now();
            let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|_| Error::RuntimeBlocked)?;
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|_| Error::RuntimeBlocked)?;
            let outcome = loop {
                if let Some(status) = child.process.try_wait().map_err(|_| Error::RuntimeBlocked)? { use std::os::unix::process::ExitStatusExt; break Ok(status.code().unwrap_or(128+status.signal().unwrap_or(0))); }
                if timeout.is_some_and(|limit| started.elapsed()>=limit) { break Err(Error::RuntimeTimeout); }
                tokio::select! { _=tokio::time::sleep(Duration::from_millis(30))=>(), _=interrupt.recv()=>break Ok(130), _=terminate.recv()=>break Ok(143) }
            };
            if child.process.try_wait().map_err(|_| Error::RuntimeBlocked)?.is_none() { let _=sys::kill_pidfd(&child.pidfd); child.process.wait().map_err(|_| Error::RuntimeBlocked)?; }
            outcome
        }.await;
        self.cleanup(&plan, &mut resources).await?;
        result
    }
}
