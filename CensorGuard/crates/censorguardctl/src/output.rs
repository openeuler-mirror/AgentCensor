use censorguard_common::protocol::{DomainInfo, Operation, PolicySource, Request, Response};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, serde::Serialize)]
pub(crate) struct Check {
    pub component: String,
    pub ok: bool,
    pub detail: String,
}

pub(crate) fn status(response: &Response) -> String {
    if !response.ok {
        return format!(
            "CensorGuard | Unavailable\n\n  ! {}\n",
            response
                .error
                .as_deref()
                .unwrap_or("Daemon rejected the status request")
        );
    }
    let state = if !response.ok || !response.daemon_ready {
        "Not ready"
    } else if response.hooks_healthy != Some(true) {
        "Needs attention"
    } else {
        "Ready"
    };
    let mut text = format!("CensorGuard | {state}\n\n");
    text.push_str("                File    Exec    Network\n");
    text.push_str(&format!(
        "  Protection    {:<8}{:<8}{}\n  Audit         {:<8}{:<8}{}\n\n",
        on_off(response.enable_file),
        on_off(response.enable_exec),
        on_off(response.enable_net),
        on_off(response.audit_file),
        on_off(response.audit_exec),
        on_off(response.audit_net),
    ));
    text.push_str(&format!(
        "  Managed       {} / {}\n  Policy        revision {} / {}\n  BPF hooks     {}\n",
        count(response.tracked, "process", "processes"),
        count(response.roots.len(), "root", "roots"),
        response.version,
        count(
            response
                .groups
                .iter()
                .filter(|group| group.name != "__base__")
                .count(),
            "custom group",
            "custom groups"
        ),
        match response.hooks_healthy {
            Some(true) => "Healthy",
            Some(false) => "Unhealthy",
            None => "Unknown",
        },
    ));
    text.push_str(&format!(
        "  Base policy   {}\n",
        response
            .config_path
            .as_deref()
            .unwrap_or("Not reported (update daemon)")
    ));
    append_domains(&mut text, response);
    if response.roots.is_empty() && response.tracked == 0 && response.daemon_ready {
        text.push_str("\n  No processes attached yet.\n");
    }
    if let Some(error) = &response.error {
        text.push_str(&format!("\n  ! {error}\n"));
    }
    if let Some(error) = &response.last_reload_error {
        text.push_str(&format!("\n  ! Last reload failed: {error}\n"));
    }
    if response.event_kernel_dropped > 0
        || response.event_reader_dropped > 0
        || response.event_subscriber_dropped > 0
    {
        text.push_str(&format!(
            "\n  ! Audit loss: kernel {}, reader {}, subscribers {}.\n",
            response.event_kernel_dropped,
            response.event_reader_dropped,
            response.event_subscriber_dropped
        ));
    }
    if response.pid_pending > 0
        || response.pid_start_update_failures > 0
        || response.pid_tracked_update_failures > 0
        || response.pid_pending_update_failures > 0
        || response.scope_policy_lookup_failures > 0
    {
        text.push_str("\n  ! Process tracking needs attention; inspect status --json.\n");
    }
    if response
        .dns
        .iter()
        .any(|domain| domain.stale || domain.error.is_some())
    {
        text.push_str("\n  ! DNS policy resolution needs attention; inspect status --json.\n");
    }
    text
}

fn append_domains(text: &mut String, response: &Response) {
    // Keep published bindings distinct from runtime scopes: an idle binding is
    // useful to operators, but must not imply any process is protected.
    let mut domains: BTreeMap<&str, (BTreeSet<&str>, Option<&DomainInfo>)> = BTreeMap::new();
    for binding in &response.bindings {
        domains.insert(
            &binding.name,
            (binding.policies.iter().map(String::as_str).collect(), None),
        );
    }
    // Also understand older daemons which report bindings only through group dumps.
    for group in &response.groups {
        for name in &group.domains {
            let entry = domains.entry(name).or_default();
            if group.name != "__base__" {
                entry.0.insert(&group.name);
            }
        }
    }
    for domain in &response.domains {
        let policies = if domain.policies.is_empty() {
            domain
                .group
                .split('+')
                .filter(|name| !name.is_empty())
                .collect()
        } else {
            domain.policies.iter().map(String::as_str).collect()
        };
        domains.insert(&domain.name, (policies, Some(domain)));
    }
    if !domains.is_empty() {
        for (name, (policies, live)) in &domains {
            let state = match live {
                Some(domain) if domain.draining => "draining; no registered roots".to_owned(),
                Some(domain) => {
                    let roots: Vec<_> = response
                        .trees
                        .iter()
                        .filter(|tree| tree.domain == **name)
                        .flat_map(|tree| &tree.nodes)
                        .filter(|node| node.root)
                        .map(|node| node.pid.to_string())
                        .collect();
                    if roots.is_empty() {
                        count(domain.roots, "registered root", "registered roots")
                    } else {
                        format!(
                            "{}; PID {}",
                            count(domain.roots, "root", "roots"),
                            roots.join(", ")
                        )
                    }
                }
                None => "configured; no processes".to_owned(),
            };
            text.push_str(&format!("\n  {name} [{state}]\n"));
            text.push_str(&format!(
                "  {} base  {} (inherited)\n",
                if policies.is_empty() {
                    "└─"
                } else {
                    "├─"
                },
                response
                    .config_path
                    .as_deref()
                    .unwrap_or("Not reported (update daemon)")
            ));
            for (index, name) in policies.iter().enumerate() {
                let group = response.groups.iter().find(|group| group.name == *name);
                text.push_str(&format!(
                    "  {} {name}  {}\n",
                    if index + 1 == policies.len() {
                        "└─"
                    } else {
                        "├─"
                    },
                    source(
                        group.and_then(|group| group.source.as_ref()),
                        response.policy_snapshot_path.as_deref()
                    )
                ));
            }
        }
    }
    let bound: BTreeSet<_> = domains
        .values()
        .flat_map(|(policies, _)| policies.iter().copied())
        .collect();
    let unbound: Vec<_> = response
        .groups
        .iter()
        .filter(|group| group.name != "__base__" && !bound.contains(group.name.as_str()))
        .collect();
    if !unbound.is_empty() {
        text.push_str("\n  Unbound policies\n");
        for (index, group) in unbound.iter().enumerate() {
            text.push_str(&format!(
                "  {} {}  {}\n",
                if index + 1 == unbound.len() {
                    "└─"
                } else {
                    "├─"
                },
                group.name,
                source(
                    group.source.as_ref(),
                    response.policy_snapshot_path.as_deref()
                )
            ));
        }
    }
    if response.groups.iter().any(|group| group.name != "__base__") {
        if let Some(path) = &response.policy_snapshot_path {
            text.push_str(&format!("\n  Group snapshot  {path}\n"));
        }
        text.push_str("  Group policies are saved copies; reapply after editing their source.\n");
    }
}

fn source(value: Option<&PolicySource>, snapshot: Option<&str>) -> String {
    match value {
        Some(PolicySource::File { path }) => path.clone(),
        Some(PolicySource::Stdin) => {
            format!("{} (from stdin)", snapshot.unwrap_or("Inline policy"))
        }
        Some(PolicySource::Api) => {
            format!("{} (from API/Web)", snapshot.unwrap_or("Inline policy"))
        }
        None => format!(
            "{} (original source unknown)",
            snapshot.unwrap_or("Saved policy")
        ),
    }
}

pub(crate) fn policy_names(policies: &[String], group: &str) -> String {
    let names = if policies.is_empty() {
        group.replace('+', " + ")
    } else {
        policies.join(" + ")
    };
    if names.is_empty() {
        "base only".into()
    } else {
        format!("base + {names}")
    }
}

pub(crate) fn operation(request: &Request, response: &Response) -> String {
    let state = if !response.ok {
        "Operation failed"
    } else {
        match request.op {
            Operation::Reload if request.dry_run => "Policy valid (no changes applied)",
            Operation::Reload => "Policy applied",
            Operation::Bind => "Policy bound",
            Operation::Remove if request.domain.is_some() => "Policy unbound",
            Operation::Remove => "Policy removed",
            Operation::Track => "Process tree attached",
            Operation::Unattach => "Process tree released",
            _ => "Operation completed",
        }
    };
    let mut text = format!("CensorGuard | {state}\n\n");
    if let Some(group) = &request.group {
        text.push_str(&format!("  Policy        {group}\n"));
    }
    if request.op == Operation::Reload {
        text.push_str(&format!(
            "  Source        {}\n",
            source(request.policy_source.as_ref(), None)
        ));
    }
    if let Some(domain) = response
        .domain
        .as_ref()
        .map(|domain| &domain.name)
        .or(request.domain.as_ref())
    {
        text.push_str(&format!("  Domain        {domain}\n"));
    }
    if let Some(pid) = request.pid {
        text.push_str(&format!("  Root PID      {pid}\n"));
    }
    if !response.ok {
        text.push_str(&format!(
            "\n  ! {}\n",
            response
                .error
                .as_deref()
                .unwrap_or("Daemon rejected the operation")
        ));
        return text;
    }
    if let Some(domain) = &response.domain {
        text.push_str(&format!(
            "  Policies      {}\n",
            policy_names(&domain.policies, &domain.group)
        ));
    }
    if request.dry_run {
        text.push_str("  Changes       None; validation only\n");
    } else if response.version > 0 {
        text.push_str(&format!("  Revision      {}\n", response.version));
    }
    if !response.daemon_ready {
        text.push_str("\n  ! Daemon is not ready; check status or doctor.\n");
    }
    text
}

fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}

fn count(value: usize, singular: &str, plural: &str) -> String {
    format!("{value} {}", if value == 1 { singular } else { plural })
}

pub(crate) fn doctor(checks: &[Check], verbose: bool) -> String {
    let passed = checks.iter().filter(|check| check.ok).count();
    let state = if passed == checks.len() {
        "All checks passed"
    } else {
        "Needs attention"
    };
    let mut text = format!("CensorGuard | {state} ({passed}/{})\n\n", checks.len());
    for (label, components) in [
        (
            "Configuration & state",
            &["config", "bpf_object", "state"][..],
        ),
        ("Kernel BPF support", &["kernel_btf", "bpf_lsm"][..]),
        ("Daemon & hooks", &["daemon", "hooks"][..]),
        ("Local channels", &["events", "launch", "dsh", "ui"][..]),
        ("gRPC adapter", &["grpc"][..]),
    ] {
        let members: Vec<_> = checks
            .iter()
            .filter(|check| components.contains(&check.component.as_str()))
            .collect();
        if members.is_empty() {
            continue;
        }
        let ok = members.iter().all(|check| check.ok);
        text.push_str(&format!(
            "  {:<24}{}\n",
            label,
            if ok { "OK" } else { "FAIL" }
        ));
    }
    for check in checks.iter().filter(|check| verbose || !check.ok) {
        text.push_str(&format!(
            "\n  {} {}: {}\n",
            if check.ok { "+" } else { "!" },
            check.component,
            check.detail
        ));
        if !check.ok {
            text.push_str(&format!("    {}\n", remedy(&check.component)));
        }
    }
    text
}

fn remedy(component: &str) -> &'static str {
    match component {
        "bpf_object" => "Rebuild and install the BPF object: make bpf, then make setup.",
        "kernel_btf" => "Use a kernel with CONFIG_DEBUG_INFO_BTF and /sys/kernel/btf/vmlinux.",
        "bpf_lsm" => "Enable bpf in the kernel LSM boot parameters, then reboot.",
        "daemon" => "Start the service; inspect journalctl -u censorguardd -n 30 if it fails.",
        "hooks" => "Inspect the daemon journal for BPF attachment errors.",
        "grpc" => "Check daemon.spawn_grpc and grpc_bin, then restart censorguardd.",
        "state" => "Check daemon.state_dir and the daemon journal for persistence errors.",
        _ => "Check the daemon service and this user's socket permissions.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_status_does_not_invent_protection_or_process_counts() {
        let text = status(&Response {
            error: Some("state unavailable".into()),
            ..Response::default()
        });
        assert!(text.contains("Unavailable"));
        assert!(text.contains("state unavailable"));
        assert!(!text.contains("Protection"));
        assert!(!text.contains("Managed"));
    }

    #[test]
    fn ready_status_does_not_imply_any_process_is_protected() {
        let response = Response {
            ok: true,
            daemon_ready: true,
            hooks_healthy: Some(true),
            ..Response::default()
        };
        let text = status(&response);
        assert!(text.contains("Ready"));
        assert!(text.contains("No processes attached yet"));
        assert!(text.contains("Protection    Off"));
        assert!(!text.contains("daemon_boot_id"));
    }

    #[test]
    fn status_keeps_material_failures_visible() {
        let response = Response {
            ok: true,
            daemon_ready: true,
            hooks_healthy: Some(false),
            event_kernel_dropped: 3,
            last_reload_error: Some("invalid policy".into()),
            ..Response::default()
        };
        let text = status(&response);
        assert!(text.contains("Needs attention"));
        assert!(text.contains("invalid policy"));
        assert!(text.contains("kernel 3"));
    }

    #[test]
    fn tracked_descendants_do_not_look_like_an_empty_daemon() {
        let text = status(&Response {
            ok: true,
            daemon_ready: true,
            tracked: 2,
            domains: vec![DomainInfo {
                name: "agent".into(),
                draining: true,
                ..DomainInfo::default()
            }],
            ..Response::default()
        });
        assert!(text.contains("draining; no registered roots"));
        assert!(!text.contains("No processes attached yet"));
    }

    #[test]
    fn doctor_hides_success_details_and_shows_actionable_failures() {
        let checks = vec![
            Check {
                component: "config".into(),
                ok: true,
                detail: "/private/path".into(),
            },
            Check {
                component: "daemon".into(),
                ok: false,
                detail: "connection refused".into(),
            },
        ];
        let text = doctor(&checks, false);
        assert!(text.contains("Needs attention (1/2)"));
        assert!(text.contains("connection refused"));
        assert!(text.contains("journalctl"));
        assert!(!text.contains("/private/path"));
        assert!(doctor(&checks, true).contains("/private/path"));
    }
}
