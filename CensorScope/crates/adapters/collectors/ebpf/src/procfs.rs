//! `/proc`-backed helpers used for attach bootstrap and identity lookup.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::SystemTime;

use model_core::container::{ContainerIdentity, ContainerRuntime};
use model_core::process::{
    HostProcessCoordinates, NamespaceIdentity, NamespaceProcessCoordinates, ProcessObservation,
};
use process_identity::{IdentityLookupError, ProcessIdentityReader, SessionIdentity};
use process_tree_snapshot_contract::snapshot::{
    ProcessSnapshot, ProcessTreeSnapshotter, TreeSnapshot,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcStatRecord {
    pid: u32,
    ppid: u32,
    start_time_ticks: u64,
}

/// Reads stable host and PID-namespace process coordinates from procfs.
pub struct ProcfsIdentityReader;

impl ProcessIdentityReader for ProcfsIdentityReader {
    fn read_identity(&self, pid: u32) -> Result<ProcessObservation, IdentityLookupError> {
        let stat = read_stat(pid)?;
        let pid_namespace = read_pid_namespace(pid);
        Ok(
            ProcessObservation::host(HostProcessCoordinates::new(stat.pid, stat.start_time_ticks))
                .with_namespace(NamespaceProcessCoordinates::new(
                    pid_namespace,
                    read_nspid_last(pid).ok().flatten().unwrap_or(stat.pid),
                    stat.start_time_ticks,
                )),
        )
    }
}

/// Resolve a PID namespace reference into current host process coordinates.
pub fn resolve_namespaced_pid(
    namespace_pid: u32,
    pid_namespace: &NamespaceIdentity,
) -> Result<ProcessObservation, String> {
    if pid_namespace.as_str() == "unknown" {
        return Err("pid namespace is unknown; namespaced PID cannot be resolved".to_string());
    }

    let mut matches = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let Ok(host_pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if read_pid_namespace(host_pid) != *pid_namespace {
            continue;
        }
        if read_nspid_last(host_pid).ok().flatten() != Some(namespace_pid) {
            continue;
        }
        let Ok(stat) = read_stat(host_pid) else {
            continue;
        };
        matches.push(
            ProcessObservation::host(HostProcessCoordinates::new(stat.pid, stat.start_time_ticks))
                .with_namespace(NamespaceProcessCoordinates::new(
                    pid_namespace.clone(),
                    namespace_pid,
                    stat.start_time_ticks,
                )),
        );
    }

    match matches.as_slice() {
        [identity] => Ok(identity.clone()),
        [] => Err(format!(
            "no host process matched namespace pid {} in {}",
            namespace_pid,
            pid_namespace.as_str()
        )),
        _ => Err(format!(
            "multiple host processes matched namespace pid {} in {}",
            namespace_pid,
            pid_namespace.as_str()
        )),
    }
}

/// Read the innermost namespace PID for a host PID.
pub fn read_process_namespace_pid(pid: u32) -> Result<u32, String> {
    read_nspid_last(pid)?.ok_or_else(|| format!("process {pid} status does not expose NSpid"))
}

/// Procfs implementation of initial process-tree discovery for `track-add`.
pub struct ProcfsTreeSnapshotter;

impl ProcessTreeSnapshotter for ProcfsTreeSnapshotter {
    type Error = String;

    fn snapshot(&self, root: &ProcessObservation) -> Result<TreeSnapshot, Self::Error> {
        let root_pid = root
            .host
            .as_ref()
            .map(|host| host.pid)
            .ok_or_else(|| "process tree snapshot requires a host PID".to_string())?;
        let stats = scan_proc_stats()?;
        if !stats.contains_key(&root_pid) {
            return Err(format!("root pid {root_pid} is not visible in /proc"));
        }

        let descendants = descendant_pids(root_pid, &stats);
        let mut processes = Vec::new();
        for pid in descendants {
            let Some(stat) = stats.get(&pid) else {
                continue;
            };
            let identity = process_observation(stat);
            let parent = if stat.pid == root_pid {
                None
            } else {
                stats.get(&stat.ppid).map(process_observation)
            };
            processes.push(ProcessSnapshot {
                identity,
                parent,
                // Snapshot-only enrichment for already-running processes.
                executable: read_link(stat.pid, "exe"),
                current_working_directory: read_link(stat.pid, "cwd"),
            });
        }

        Ok(TreeSnapshot {
            root: root.clone(),
            captured_at: SystemTime::now(),
            processes,
        })
    }
}

fn process_observation(stat: &ProcStatRecord) -> ProcessObservation {
    ProcessObservation::host(HostProcessCoordinates::new(stat.pid, stat.start_time_ticks))
        .with_namespace(NamespaceProcessCoordinates::new(
            read_pid_namespace(stat.pid),
            read_nspid_last(stat.pid).ok().flatten().unwrap_or(stat.pid),
            stat.start_time_ticks,
        ))
}

fn scan_proc_stats() -> Result<BTreeMap<u32, ProcStatRecord>, String> {
    let mut stats = BTreeMap::new();
    for entry in std::fs::read_dir("/proc").map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Ok(stat) = read_stat(pid) {
            stats.insert(pid, stat);
        }
    }
    Ok(stats)
}

fn descendant_pids(root_pid: u32, stats: &BTreeMap<u32, ProcStatRecord>) -> BTreeSet<u32> {
    let mut descendants = BTreeSet::new();
    descendants.insert(root_pid);
    let mut changed = true;
    while changed {
        changed = false;
        for stat in stats.values() {
            if descendants.contains(&stat.ppid) && descendants.insert(stat.pid) {
                changed = true;
            }
        }
    }
    descendants
}

fn read_stat(pid: u32) -> Result<ProcStatRecord, IdentityLookupError> {
    let path = format!("/proc/{pid}/stat");
    let raw = std::fs::read_to_string(path).map_err(|error| {
        if proc_entry_gone(&error) {
            IdentityLookupError::NotFound { pid }
        } else if error.kind() == std::io::ErrorKind::PermissionDenied {
            IdentityLookupError::PermissionDenied { pid }
        } else {
            IdentityLookupError::Incomplete {
                pid,
                detail: error.to_string(),
            }
        }
    })?;
    let close_paren = raw
        .rfind(')')
        .ok_or_else(|| IdentityLookupError::Incomplete {
            pid,
            detail: "invalid /proc stat format".to_string(),
        })?;
    let remainder = raw
        .get(close_paren + 2..)
        .ok_or_else(|| IdentityLookupError::Incomplete {
            pid,
            detail: "missing stat fields".to_string(),
        })?;
    let fields = remainder.split_whitespace().collect::<Vec<_>>();
    let ppid = fields
        .get(1)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| IdentityLookupError::Incomplete {
            pid,
            detail: "missing ppid".to_string(),
        })?;
    let start_time_ticks = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| IdentityLookupError::Incomplete {
            pid,
            detail: "missing start_time_ticks".to_string(),
        })?;
    Ok(ProcStatRecord {
        pid,
        ppid,
        start_time_ticks,
    })
}

fn read_pid_namespace(pid: u32) -> NamespaceIdentity {
    let path = PathBuf::from(format!("/proc/{pid}/ns/pid"));
    let value = std::fs::read_link(path)
        .map(|value| value.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    NamespaceIdentity::new(value)
}

/// Resolve a process's container identity from its cgroup; `None` for host
/// processes or unrecognized runtime layouts. Pass the host pid (after NSpid
/// mapping) so the cgroup path keeps the full runtime-assigned id.
pub fn read_container_identity(pid: u32) -> Option<ContainerIdentity> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    parse_container_identity(&content)
}

/// Parse a `/proc/<pid>/cgroup` file body into a container identity.
///
/// Recognizes Docker, containerd/Kata and Kubernetes layouts on cgroup v1
/// (`N:controllers:/path`) and v2 (`0::/path`); pod-UID ancestors are never
/// mistaken for the container id. Kata guest cgroupfs layouts fall to the
/// final leaf fallback.
pub fn parse_container_identity(cgroup_file: &str) -> Option<ContainerIdentity> {
    for line in cgroup_file.lines() {
        // "N:controllers:/path" (v1) or "0::/path" (v2); cgroup paths have no ':'.
        let Some(path) = line.splitn(3, ':').nth(2) else {
            continue;
        };
        if let Some(identity) = container_identity_from_path(path) {
            return Some(identity);
        }
    }
    None
}

/// Scope prefixes used by systemd cgroup drivers: `<prefix><id>.scope`.
const SCOPE_RUNTIMES: [(&str, ContainerRuntime); 2] = [
    ("docker-", ContainerRuntime::Docker),
    ("cri-containerd-", ContainerRuntime::Containerd),
];

fn container_identity_from_path(path: &str) -> Option<ContainerIdentity> {
    let mut prev_was_docker = false;
    let mut prev_was_pod_dir = false;
    for segment in path.split('/') {
        // Kubernetes pod ancestor: never take it as the container id; remember it
        // for the container leaf that follows.
        if is_pod_segment(segment) {
            prev_was_pod_dir = true;
            prev_was_docker = false;
            continue;
        }
        if let Some(scope) = segment.strip_suffix(".scope") {
            for (prefix, runtime) in SCOPE_RUNTIMES {
                if let Some(id) = scope.strip_prefix(prefix)
                    && is_container_id(id)
                {
                    return Some(ContainerIdentity::new(runtime, id));
                }
            }
        }
        if prev_was_docker && is_container_id(segment) {
            return Some(ContainerIdentity::new(ContainerRuntime::Docker, segment));
        }
        // A bare hex leaf after a pod dir does not name its runtime; tag it `K8s`.
        if prev_was_pod_dir && is_container_id(segment) {
            return Some(ContainerIdentity::new(ContainerRuntime::K8s, segment));
        }
        prev_was_docker = segment == "docker";
        prev_was_pod_dir = false;
    }
    // Fallback: kata-agent (in-guest) exposes workloads cgroupfs-style as
    // `/<containerd-namespace>/<container-id>` with no runtime prefix or
    // `.scope` suffix; a bare container-id leaf is tagged Containerd. The
    // >=12-hex `is_container_id` shape keeps guest system paths out.
    if let Some(leaf) = path.rsplit('/').find(|segment| !segment.is_empty())
        && is_container_id(leaf)
    {
        return Some(ContainerIdentity::new(ContainerRuntime::Containerd, leaf));
    }
    None
}

/// Extract a Kubernetes pod UID from a cgroup path segment, if present.
///
/// systemd driver encodes it as `kubepods-<qos->pod<uid_with_underscores>.slice`;
/// the cgroupfs driver as a plain `pod<uid>` directory.
fn is_pod_segment(segment: &str) -> bool {
    if segment.starts_with("kubepods")
        && let Some(rest) = segment.strip_suffix(".slice")
        && let Some(index) = rest.rfind("pod")
    {
        let uid = rest[index + 3..].replace('_', "-");
        if is_pod_uid(&uid) {
            return true;
        }
    }
    if let Some(raw) = segment.strip_prefix("pod")
        && is_pod_uid(raw)
    {
        return true;
    }
    false
}

/// Heuristic: does this cgroup file look containerized even though
/// [`parse_container_identity`] extracted nothing?
///
/// Security backstop, not an identity source. When a runtime lays out cgroups
/// in a shape the parser does not know (e.g. an unmapped kata-agent guest
/// layout), the process must NOT be silently treated as a host process —
/// callers use this to refuse host-level trust and to log the degradation
/// loudly instead. False positives only cost a warning plus stricter
/// matching; false negatives would silently weaken cross-container isolation.
pub fn cgroup_looks_containerized(cgroup_file: &str) -> bool {
    for line in cgroup_file.lines() {
        let Some(path) = line.splitn(3, ':').nth(2) else {
            continue;
        };
        if has_containerd_namespace_layout(path) {
            return true;
        }
        for segment in path.split('/') {
            if segment.is_empty() {
                continue;
            }
            let lower = segment.to_ascii_lowercase();
            // Unknown runtime markers stay a security-only backstop: they keep
            // unsupported container layouts from inheriting host trust, but
            // do not resolve a ContainerIdentity above.
            if lower.contains("docker")
                || lower.contains("containerd")
                || lower.contains("crio")
                || lower.contains("libpod")
                || lower.contains("kube")
                || lower.starts_with("kata")
                || lower == "vc"
                || is_pod_segment(segment)
            {
                return true;
            }
        }
    }
    false
}

/// kata-agent/containerd may expose a workload as
/// `/<containerd-namespace>/<container-id>` without a runtime prefix. Keep
/// identity parsing strict, but treat even a non-hex leaf as containerized so
/// an unresolved workload cannot inherit host-level trust.
fn has_containerd_namespace_layout(path: &str) -> bool {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    matches!(segments.next(), Some("default" | "k8s.io")) && segments.next().is_some()
}

fn is_container_id(value: &str) -> bool {
    value.len() >= 12 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// RFC-4122 pod-UID shape check.
fn is_pod_uid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn read_nspid_last(pid: u32) -> Result<Option<u32>, String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|error| error.to_string())?;
    Ok(raw.lines().find_map(|line| {
        line.strip_prefix("NSpid:").and_then(|value| {
            value
                .split_whitespace()
                .last()
                .and_then(|raw| raw.parse::<u32>().ok())
        })
    }))
}

fn read_link(pid: u32, entry: &str) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/{entry}"))
        .ok()
        .map(|value| value.display().to_string())
}

pub fn read_process_cwd(pid: u32) -> Option<String> {
    read_link(pid, "cwd")
}

fn proc_entry_gone(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

/// Reads the value of one environment variable from `/proc/<pid>/environ`.
///
/// The file is a NUL-separated list of `KEY=VALUE` entries. Returns `None`
/// when the process is gone, unreadable, or does not carry the variable.
pub fn read_process_env(pid: u32, name: &str) -> Option<String> {
    let path = format!("/proc/{pid}/environ");
    let content = std::fs::read(&path).ok()?;
    let needle = format!("{name}=");
    for entry in content.split(|byte| *byte == 0) {
        if let Some(value) = entry.strip_prefix(needle.as_bytes())
            && !value.is_empty()
        {
            return Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    None
}

/// Reads the session identifier from a process environment, if present.
pub fn read_process_session(pid: u32, env_name: &str) -> Option<SessionIdentity> {
    read_process_env(pid, env_name)
        .filter(|value| !value.is_empty())
        .map(SessionIdentity::new)
}

#[cfg(test)]
mod tests {
    use process_identity::ProcessIdentityReader;
    use process_tree_snapshot_contract::snapshot::ProcessTreeSnapshotter;

    use super::{
        ProcfsIdentityReader, ProcfsTreeSnapshotter, parse_container_identity, read_process_env,
        read_process_session,
    };
    use model_core::container::ContainerRuntime;

    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parse_container_docker_v2_systemd() {
        let cgroup = format!("0::/system.slice/docker-{ID}.scope\n");
        let identity = parse_container_identity(&cgroup).expect("docker id");
        assert_eq!(identity.runtime, ContainerRuntime::Docker);
        assert_eq!(identity.container_id, ID);
    }

    #[test]
    fn parse_container_docker_v1_cgroupfs() {
        let cgroup = format!("12:pids:/docker/{ID}\n11:memory:/docker/{ID}\n");
        let identity = parse_container_identity(&cgroup).expect("docker id");
        assert_eq!(identity.runtime, ContainerRuntime::Docker);
        assert_eq!(identity.container_id, ID);
    }

    const POD_UID: &str = "2ee7d8a2-e832-4a13-b26c-02ad9ae4a8f6";

    #[test]
    fn parse_container_cri_containerd_systemd() {
        let uid_underscored = POD_UID.replace('-', "_");
        let cgroup = format!(
            "0::/kubepods.slice/kubepods-besteffort.slice/\
             kubepods-besteffort-pod{uid_underscored}.slice/cri-containerd-{ID}.scope\n"
        );
        let identity = parse_container_identity(&cgroup).expect("containerd id");
        assert_eq!(identity.runtime, ContainerRuntime::Containerd);
        assert_eq!(identity.container_id, ID);
    }

    #[test]
    fn unsupported_runtime_layouts_do_not_resolve_identity() {
        let uid_underscored = POD_UID.replace('-', "_");
        let crio = format!(
            "0::/kubepods.slice/kubepods-burstable.slice/\
             kubepods-burstable-pod{uid_underscored}.slice/crio-{ID}.scope\n"
        );
        let scoped = format!("0::/machine.slice/libpod-{ID}.scope\n");
        let cgroupfs = format!("12:pids:/libpod_parent/libpod-{ID}\n");

        assert!(parse_container_identity(&crio).is_none());
        assert!(parse_container_identity(&scoped).is_none());
        assert!(parse_container_identity(&cgroupfs).is_none());
    }

    #[test]
    fn parse_container_kubepods_cgroupfs_leaf() {
        let cgroup = format!("12:pids:/kubepods/besteffort/pod{POD_UID}/{ID}\n");
        let identity = parse_container_identity(&cgroup).expect("cri leaf id");
        assert_eq!(identity.runtime, ContainerRuntime::K8s);
        assert_eq!(identity.container_id, ID);
    }

    #[test]
    fn parse_container_pod_uid_is_never_the_container_id() {
        let cgroup = format!("0::/kubepods/besteffort/pod{POD_UID}\n");
        assert!(parse_container_identity(&cgroup).is_none());

        let uid_underscored = POD_UID.replace('-', "_");
        let systemd_only_pod = format!(
            "0::/kubepods.slice/kubepods-besteffort.slice/\
             kubepods-besteffort-pod{uid_underscored}.slice\n"
        );
        assert!(parse_container_identity(&systemd_only_pod).is_none());
    }

    #[test]
    fn parse_container_kata_agent_guest_layout() {
        // kata-agent lays workload cgroups out cgroup-v2 cgroupfs-style as
        // `/<containerd-namespace>/<container-id>`; it must be read guest-side,
        // since inside a container the cgroup namespace masks the path to `0::/`.
        let k8s = format!("0::/k8s.io/{ID}\n");
        let identity = parse_container_identity(&k8s).expect("kata k8s.io leaf");
        assert_eq!(identity.runtime, ContainerRuntime::Containerd);
        assert_eq!(identity.container_id, ID);

        let default_ns = format!("0::/default/{ID}\n");
        let identity = parse_container_identity(&default_ns).expect("kata default leaf");
        assert_eq!(identity.runtime, ContainerRuntime::Containerd);
        assert_eq!(identity.container_id, ID);
    }

    #[test]
    fn parse_container_kata_agent_guest_v1_layout() {
        let cgroup = format!(
            "12:pids:/k8s.io/{ID}\n\
             11:memory:/k8s.io/{ID}\n\
             10:devices:/k8s.io/{ID}\n"
        );
        let identity = parse_container_identity(&cgroup).expect("kata v1 k8s.io leaf");
        assert_eq!(identity.runtime, ContainerRuntime::Containerd);
        assert_eq!(identity.container_id, ID);

        let default_ns = format!(
            "12:pids:/default/{ID}\n\
             11:memory:/default/{ID}\n"
        );
        let identity = parse_container_identity(&default_ns).expect("kata v1 default leaf");
        assert_eq!(identity.runtime, ContainerRuntime::Containerd);
        assert_eq!(identity.container_id, ID);
    }

    #[test]
    fn parse_container_kata_guest_system_paths_are_none() {
        // Guest system services and init are not containers: their leaves are not
        // container-id shaped.
        assert!(parse_container_identity("0::/init.scope\n").is_none());
        assert!(parse_container_identity("0::/system.slice/kata-agent.service\n").is_none());
        assert!(parse_container_identity("0::/system.slice/systemd-journald.service\n").is_none());
    }

    #[test]
    fn parse_container_host_is_none() {
        let cgroup = "0::/user.slice/user-1000.slice/session-3.scope\n";
        assert!(parse_container_identity(cgroup).is_none());
    }

    #[test]
    fn containerized_heuristic_flags_unparsed_layouts_but_not_host() {
        use super::cgroup_looks_containerized;
        assert!(cgroup_looks_containerized("0::/vc/abc123\n"));
        assert!(cgroup_looks_containerized("0::/kata_sandbox/agent\n"));
        assert!(cgroup_looks_containerized("0::/default/guestprobe\n"));
        assert!(cgroup_looks_containerized("0::/k8s.io/guestprobe\n"));
        assert!(cgroup_looks_containerized(&format!(
            "0::/kubepods/besteffort/pod{POD_UID}\n"
        )));
        assert!(parse_container_identity("0::/default/guestprobe\n").is_none());
        assert!(parse_container_identity("0::/k8s.io/guestprobe\n").is_none());
        assert!(!cgroup_looks_containerized(
            "0::/user.slice/user-1000.slice/session-3.scope\n"
        ));
        assert!(!cgroup_looks_containerized("0::/init.scope\n"));
        assert!(!cgroup_looks_containerized("0::/default\n"));
        assert!(!cgroup_looks_containerized(""));
    }

    #[test]
    fn parse_container_garbage_is_none() {
        assert!(parse_container_identity("").is_none());
        assert!(parse_container_identity("no colons here\n").is_none());
        assert!(parse_container_identity("0::/system.slice/docker-short.scope\n").is_none());
    }

    #[test]
    fn identity_reader_reads_current_process() {
        let identity = ProcfsIdentityReader
            .read_identity(std::process::id())
            .unwrap();
        let host = identity.host.expect("host coordinates");
        assert_eq!(host.pid, std::process::id());
        assert!(host.start_time_ticks > 0);
    }

    #[test]
    fn tree_snapshot_contains_root_process() {
        let identity = ProcfsIdentityReader
            .read_identity(std::process::id())
            .unwrap();
        let snapshot = ProcfsTreeSnapshotter.snapshot(&identity).unwrap();
        assert!(snapshot.processes.iter().any(|process| {
            process
                .identity
                .host
                .as_ref()
                .is_some_and(|host| host.pid == std::process::id())
        }));
    }

    #[test]
    fn process_env_returns_absent_variables_as_none() {
        assert!(read_process_env(std::process::id(), "CENSORSCOPE_ABSENT_VAR_XYZ").is_none());
        assert!(read_process_session(std::process::id(), "CENSORSCOPE_ABSENT_VAR_XYZ").is_none());
    }

    #[test]
    fn process_env_reads_present_variables() {
        assert!(read_process_env(std::process::id(), "PATH").is_some());
    }
}
