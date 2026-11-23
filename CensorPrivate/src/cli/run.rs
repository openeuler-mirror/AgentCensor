#[cfg(target_os = "linux")]
mod linux {
    use censorprivacy::{
        control,
        error::{Error, Result},
        json,
        runtime::{LinuxRuntime, RuntimePlan, RuntimeSpec, ENV_ALLOWLIST},
    };
    use clap::Args;
    use std::{collections::BTreeMap, path::PathBuf, time::Duration};
    #[derive(Args)]
    pub struct Options {
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long, alias = "state-file")]
        run_state_file: Option<PathBuf>,
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        uid: u32,
        #[arg(long)]
        gid: u32,
        #[arg(long)]
        gateway_port: u16,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        read_only_path: Vec<PathBuf>,
        #[arg(long, value_name = "NAME=VALUE")]
        env: Vec<String>,
        #[arg(long)]
        timeout: Option<f64>,
        #[arg(long)]
        dry_run: bool,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    }
    async fn run(cli: Options) -> Result<i32> {
        let timeout = cli
            .timeout
            .map(Duration::try_from_secs_f64)
            .transpose()
            .map_err(|_| Error::RuntimeBlocked)?;
        if timeout.is_some_and(|duration| duration.is_zero()) {
            return Err(Error::RuntimeBlocked);
        }
        let mut environment = BTreeMap::new();
        for definition in cli.env {
            let (name, value) = definition.split_once('=').ok_or(Error::RuntimeBlocked)?;
            if !ENV_ALLOWLIST.contains(&name)
                || environment
                    .insert(name.to_owned(), value.to_owned())
                    .is_some()
            {
                return Err(Error::RuntimeBlocked);
            }
        }
        let mut run_id = cli.run_id;
        if let Some(path) = cli.run_state_file {
            let bytes = control::private_file(&path, cli.uid, 8192)?;
            let state = json::from_slice(&bytes)?;
            let expected = state
                .get("run_id")
                .and_then(serde_json::Value::as_str)
                .ok_or(Error::RuntimeBlocked)?;
            if state.as_object().is_none_or(|object| object.len() != 1)
                || run_id.as_ref().is_some_and(|id| id != expected)
            {
                return Err(Error::RuntimeBlocked);
            }
            run_id = Some(expected.to_owned());
        }
        let run_id = run_id.ok_or(Error::RuntimeBlocked)?;
        let mut spec = RuntimeSpec {
            run_id: run_id.clone(),
            command: cli.command,
            workspace: cli.workspace,
            gateway_port: cli.gateway_port,
            uid: cli.uid,
            gid: cli.gid,
            environment,
            ipv4_subnet: None,
            guard_launcher: None,
            guard_group: "censorprivacy".into(),
            read_only_paths: cli.read_only_path,
        };
        let plan = RuntimePlan::new(spec.clone())?;
        if cli.dry_run {
            println!(
                "{}",
                serde_json::to_string_pretty(&plan.document())
                    .map_err(|_| Error::RuntimeBlocked)?
            );
            return Ok(0);
        }
        let token = control::token(&cli.token_file.ok_or(Error::RuntimeBlocked)?, cli.uid)?;
        for (name, value) in [
            ("OPENAI_BASE_URL", plan.gateway_url() + "/v1"),
            ("ANTHROPIC_BASE_URL", plan.gateway_url()),
            ("OPENAI_API_KEY", token.to_string()),
            ("ANTHROPIC_API_KEY", token.to_string()),
            ("CENSORPRIVACY_GATEWAY", plan.gateway_url()),
            ("CENSORPRIVACY_URL", plan.gateway_url()),
            (
                "CENSORPRIVACY_GATEWAY_IP",
                plan.gateway_address().to_string(),
            ),
            ("CENSORPRIVACY_TOKEN", token.to_string()),
            ("CENSORPRIVACY_RUN_ID", run_id.clone()),
        ] {
            spec.environment.insert(name.into(), value);
        }
        let executable = std::env::current_exe().map_err(|_| Error::RuntimeBlocked)?;
        let port = cli.gateway_port;
        LinuxRuntime::default()
            .run(
                spec,
                &executable,
                move |_| async move {
                    let document = control::readiness_document(port, &token).await?;
                    Ok(document["status"] == "ready" && document["run_id"] == run_id)
                },
                timeout,
                Duration::from_secs(15),
            )
            .await
    }
    pub fn bootstrap(args: &[String]) -> i32 {
        // Do not create threads before mount/credential/seccomp preparation.
        let result = (|| {
            if args.len() != 4 {
                return Err(Error::RuntimeBlocked);
            }
            censorprivacy::runtime::bootstrap::run(
                args[2].parse().map_err(|_| Error::RuntimeBlocked)?,
                args[3].parse().map_err(|_| Error::RuntimeBlocked)?,
            )
        })();
        if result.is_err() {
            eprintln!("{{\"error\":{{\"code\":\"runtime_blocked\"}}}}");
        }
        125
    }
    pub fn execute(cli: Options) -> i32 {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return 125,
        };
        match runtime.block_on(run(cli)) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("{{\"error\":{{\"code\":\"{}\"}}}}", error.code());
                if error == Error::RuntimeTimeout {
                    124
                } else {
                    125
                }
            }
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::Options;
#[cfg(target_os = "linux")]
pub fn run(options: Options) -> i32 {
    linux::execute(options)
}
#[cfg(not(target_os = "linux"))]
#[derive(clap::Args)]
pub struct Options {}
#[cfg(not(target_os = "linux"))]
pub fn run(_: Options) -> i32 {
    eprintln!("Linux required");
    125
}

#[cfg(target_os = "linux")]
pub fn bootstrap(args: &[String]) -> i32 {
    linux::bootstrap(args)
}
