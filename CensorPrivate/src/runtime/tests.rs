use super::*;
use std::sync::Mutex;
fn spec() -> RuntimeSpec {
    RuntimeSpec {
        run_id: "runtime-test".into(),
        command: vec!["/bin/true".into()],
        workspace: "/tmp/cp-workspace".into(),
        gateway_port: 8765,
        uid: 65534,
        gid: 65534,
        environment: BTreeMap::from([("TOKEN".into(), "sensitive-test-environment".into())]),
        ipv4_subnet: None,
        guard_launcher: None,
        guard_group: "censorprivacy".into(),
        read_only_paths: Vec::new(),
    }
}
#[test]
fn plan_has_only_gateway_egress_and_never_serializes_environment_values() {
    let plan = RuntimePlan::new(spec()).unwrap();
    let document = plan.document();
    let encoded = document.to_string();
    assert!(!encoded.contains("sensitive-test-environment"));
    assert_eq!(document["setup"].as_array().unwrap().len(), 10);
    assert_eq!(
        document["subnet"],
        RuntimePlan::new(spec()).unwrap().document()["subnet"]
    );
    let rules = document["setup"][9]["stdin"].as_str().unwrap();
    assert!(rules.contains("policy drop"));
    assert!(rules.contains("tcp dport 8765"));
    assert!(!rules.contains("masquerade"));
    assert!(!rules.contains("udp dport 53"));
    for workspace in [
        "/",
        "/proc/private",
        "/sys/a",
        "/dev/x",
        "relative",
        "/tmp/../etc",
    ] {
        let mut spec = spec();
        spec.workspace = workspace.into();
        assert!(RuntimePlan::new(spec).is_err());
    }
    for readonly in ["/", "/etc", "/root", "/proc/a", "/var", "relative"] {
        let mut spec = spec();
        spec.read_only_paths = vec![readonly.into()];
        assert!(RuntimePlan::new(spec).is_err());
    }
    let mut spec = spec();
    spec.uid = 0;
    assert!(RuntimePlan::new(spec).is_err());
}
struct Runner {
    commands: Mutex<Vec<Vec<String>>>,
    fail_at: usize,
    changed: bool,
    fail_pids: bool,
}
impl RuntimeRunner for Runner {
    fn namespace_identity(&self, _: &str) -> Result<(u64, u64)> {
        if self.changed {
            Ok((2, 2))
        } else {
            Ok((1, 1))
        }
    }
    async fn execute(&self, command: &RuntimeCommand) -> Result<CommandResult> {
        let mut commands = self.commands.lock().unwrap();
        commands.push(command.argv.clone());
        let failed = commands.len() == self.fail_at
            || self.fail_pids && command.argv.get(2).is_some_and(|arg| arg == "pids");
        let stdout = if command.argv.get(1) == Some(&"-j".into()) {
            b"[]".to_vec()
        } else {
            Vec::new()
        };
        Ok(CommandResult {
            success: !failed,
            stdout,
        })
    }
}
#[tokio::test]
async fn setup_failure_cleans_owned_resources_and_preserves_other_domains() {
    for fail_at in 1..=10 {
        let runtime = LinuxRuntime::with_runner(Runner {
            commands: Mutex::new(Vec::new()),
            fail_at,
            changed: false,
            fail_pids: false,
        });
        let plan = RuntimePlan::new(spec()).unwrap();
        let mut resources = Resources::default();
        assert!(runtime.prepare(&plan, &mut resources).await.is_err());
        runtime.cleanup(&plan, &mut resources).await.unwrap();
        assert!(!resources.namespace && !resources.link && !resources.table);
        let commands = runtime.runner.commands.lock().unwrap();
        let cleanup = &commands[fail_at..];
        if fail_at > 1 {
            assert_eq!(&cleanup[0][..3], ["ip", "netns", "pids"]);
            assert_eq!(&cleanup[1][..3], ["ip", "netns", "delete"]);
        }
        assert!(cleanup.iter().all(|argv| !argv.contains(&"flush".into())));
    }
}
#[tokio::test]
async fn changed_namespace_or_failed_population_check_keeps_firewall() {
    for (changed, fail_pids) in [(true, false), (false, true)] {
        let runtime = LinuxRuntime::with_runner(Runner {
            commands: Mutex::new(Vec::new()),
            fail_at: usize::MAX,
            changed,
            fail_pids,
        });
        let plan = RuntimePlan::new(spec()).unwrap();
        let mut resources = Resources {
            namespace: true,
            link: true,
            table: true,
            identity: Some((1, 1)),
        };
        assert_eq!(
            runtime.cleanup(&plan, &mut resources).await,
            Err(Error::RuntimeCleanupFailed)
        );
        assert!(resources.namespace && resources.link && resources.table);
        assert!(!runtime
            .runner
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|argv| argv.contains(&"delete".into())));
    }
}
struct VethRace {
    host: String,
    disappears: bool,
    queries: Mutex<usize>,
    table_deleted: Mutex<bool>,
}
impl RuntimeRunner for VethRace {
    async fn execute(&self, command: &RuntimeCommand) -> Result<CommandResult> {
        if command.argv.get(1).is_some_and(|arg| arg == "-j") {
            let mut queries = self.queries.lock().unwrap();
            *queries += 1;
            let value = if *queries == 1 || !self.disappears {
                json!([{"ifname":self.host}])
            } else {
                json!([])
            };
            return Ok(CommandResult {
                success: true,
                stdout: serde_json::to_vec(&value).unwrap(),
            });
        }
        let success = command.argv.first().is_some_and(|arg| arg == "nft");
        if success {
            *self.table_deleted.lock().unwrap() = true;
        }
        Ok(CommandResult {
            success,
            stdout: Vec::new(),
        })
    }
}
#[tokio::test]
async fn asynchronous_veth_disappearance_is_confirmed_before_removing_firewall() {
    for disappears in [true, false] {
        let plan = RuntimePlan::new(spec()).unwrap();
        let runtime = LinuxRuntime::with_runner(VethRace {
            host: plan.host_interface.clone(),
            disappears,
            queries: Mutex::new(0),
            table_deleted: Mutex::new(false),
        });
        let mut resources = Resources {
            link: true,
            table: true,
            ..Default::default()
        };
        let result = runtime.cleanup(&plan, &mut resources).await;
        assert_eq!(result.is_ok(), disappears);
        assert_eq!(*runtime.runner.table_deleted.lock().unwrap(), disappears);
        assert_eq!(*runtime.runner.queries.lock().unwrap(), 2);
    }
}
