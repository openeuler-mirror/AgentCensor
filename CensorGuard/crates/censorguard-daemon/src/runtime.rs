use crate::procfs;
use censorguard_common::abi::TrackValue;
use censorguard_common::protocol::{
    DnsDomainInfo, DomainInfo, DomainTree, GroupDump, IntentDecision, PolicyDefinition,
    SecurityIntent, TreeNode,
};
use censorguard_kernel::{InstalledPolicy, KernelError, NativeKernel};
use censorguard_policy::{
    CompiledPolicy, DnsCache, DnsResolution, GroupPolicy, PolicyError, compile, compile_yaml, load,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("kernel operation failed: {0}")]
    Kernel(#[from] KernelError),
    #[error("process inspection failed: {0}")]
    Process(#[from] io::Error),
    #[error("policy compilation failed: {0}")]
    Policy(#[from] PolicyError),
    #[error("domain {0:?} is not defined by policy")]
    UnknownDomain(String),
    #[error("domain {domain:?} references uninstalled group {group:?}")]
    MissingGroup { domain: String, group: String },
    #[error("pid {0} is already a registered root")]
    DuplicateRoot(u32),
    #[error("pid {0} is not a registered root")]
    UnknownRoot(u32),
    #[error("runtime state lock is poisoned")]
    Poisoned,
    #[error("uid {peer_uid} cannot track pid {pid}, which belongs to uid {owner_uid}")]
    UnauthorizedPid {
        peer_uid: u32,
        pid: u32,
        owner_uid: u32,
    },
    #[error("uid {peer_uid} cannot untrack root pid {pid}")]
    UnauthorizedRoot { peer_uid: u32, pid: u32 },
    #[error("reload would remove active domain {0:?}")]
    ActiveDomainRemoved(String),
    #[error("reload would remove policy group {group:?} used by active scope {scope:?}")]
    ActiveGroupRemoved { scope: String, group: String },
    #[error("cannot change explicit/implicit domain schema while roots are active")]
    ActiveDomainSchemaChange,
    #[error("group reload requires policy_yaml content")]
    MissingGroupYaml,
    #[error("policy revision conflict: expected {expected}, current {current}")]
    RevisionConflict { expected: u64, current: u64 },
    #[error("policy revision {0} does not exist")]
    UnknownRevision(u64),
    #[error("policy revision store failed: {0}")]
    RevisionStore(String),
    #[error("policy group {0:?} is not in the DSH self-serve whitelist")]
    GroupNotAllowed(String),
    #[error("instance hint {0:?} is invalid")]
    InvalidInstanceHint(String),
}

#[derive(Debug)]
struct RootState {
    domain: String,
    start_time: u64,
    owner_uid: u32,
}

#[derive(Debug)]
struct DomainState {
    id: u64,
    name: String,
    group: String,
    slot: u32,
    roots: BTreeSet<u32>,
    draining: bool,
    policy_managed: bool,
}

#[derive(Debug)]
struct State {
    next_domain_id: u64,
    roots: BTreeMap<u32, RootState>,
    domains: BTreeMap<String, DomainState>,
    names_by_id: BTreeMap<u64, String>,
}

#[derive(Debug)]
struct PolicyState {
    policy: Arc<CompiledPolicy>,
    installed: InstalledPolicy,
    active_bank: u32,
    version: u32,
    reload_generation: u64,
}

#[derive(Debug)]
struct DnsState {
    cache: DnsCache,
    status: Vec<DnsResolution>,
}

#[derive(Debug)]
struct RevisionState {
    snapshots: BTreeMap<u64, CompiledPolicy>,
    idempotency: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default)]
struct ReloadObservation {
    time_ms: Option<u64>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwitchState {
    pub enables: [bool; 3],
    pub audits: [bool; 3],
}

impl Default for SwitchState {
    fn default() -> Self {
        Self {
            enables: [true; 3],
            audits: [false; 3],
        }
    }
}

pub struct Runtime {
    kernel: Arc<NativeKernel>,
    config_path: PathBuf,
    policy_state: RwLock<PolicyState>,
    switches: Mutex<SwitchState>,
    reload_lock: Mutex<()>,
    reload_observation: Mutex<ReloadObservation>,
    dns_state: Mutex<DnsState>,
    revisions: Mutex<RevisionState>,
    revision_dir: PathBuf,
    state: Mutex<State>,
    boot_id: String,
    hooks_healthy: bool,
    dsh_self_groups: BTreeSet<String>,
}

pub struct RuntimeInit {
    pub kernel: Arc<NativeKernel>,
    pub config_path: PathBuf,
    pub policy: Arc<CompiledPolicy>,
    pub installed: InstalledPolicy,
    pub dns_cache: DnsCache,
    pub dns_status: Vec<DnsResolution>,
    pub revision_dir: PathBuf,
    pub initial_revision: u32,
    pub boot_id: String,
    pub hooks_healthy: bool,
    pub dsh_self_groups: BTreeSet<String>,
}

pub struct TrackResult {
    pub domain: DomainInfo,
    pub seeded: usize,
}

pub struct ReloadResult {
    pub generation: u64,
    pub version: u32,
    pub active_bank: u32,
    pub changed: Vec<String>,
    pub enables: [bool; 3],
    pub audits: [bool; 3],
}

pub struct RuntimeStatus {
    pub roots: Vec<u32>,
    pub tracked: usize,
    pub pid_pending: usize,
    pub pid_generations: usize,
    pub pid_tracked_capacity: usize,
    pub pid_pending_capacity: usize,
    pub pid_start_update_failures: u64,
    pub pid_tracked_update_failures: u64,
    pub pid_pending_update_failures: u64,
    pub pid_pending_fallbacks: u64,
    pub scope_policy_lookup_failures: u64,
    pub scopes: usize,
    pub scope_capacity: usize,
    pub domains: Vec<DomainInfo>,
    pub reload_generation: u64,
    pub version: u32,
    pub active_bank: u32,
    pub enables: [bool; 3],
    pub audits: [bool; 3],
    pub last_reload_time_ms: Option<u64>,
    pub last_reload_error: Option<String>,
}

pub type PolicyDump = (SwitchState, Vec<GroupDump>, Vec<DnsDomainInfo>);

impl Runtime {
    pub fn new(init: RuntimeInit) -> Result<Self, RuntimeError> {
        let RuntimeInit {
            kernel,
            config_path,
            policy,
            installed,
            dns_cache,
            dns_status,
            revision_dir,
            initial_revision,
            boot_id,
            hooks_healthy,
            dsh_self_groups,
        } = init;
        let runtime = Self {
            kernel,
            config_path,
            policy_state: RwLock::new(PolicyState {
                policy: Arc::clone(&policy),
                installed,
                active_bank: 0,
                version: initial_revision,
                reload_generation: u64::from(initial_revision.saturating_sub(1)),
            }),
            switches: Mutex::new(SwitchState::default()),
            reload_lock: Mutex::new(()),
            reload_observation: Mutex::new(ReloadObservation::default()),
            dns_state: Mutex::new(DnsState {
                cache: dns_cache,
                status: dns_status,
            }),
            revisions: Mutex::new(RevisionState {
                snapshots: BTreeMap::from([(u64::from(initial_revision), policy.as_ref().clone())]),
                idempotency: BTreeMap::new(),
            }),
            revision_dir,
            state: Mutex::new(State {
                next_domain_id: 1,
                roots: BTreeMap::new(),
                domains: BTreeMap::new(),
                names_by_id: BTreeMap::new(),
            }),
            boot_id,
            hooks_healthy,
            dsh_self_groups,
        };
        runtime.persist_revision_snapshot(initial_revision, &policy)?;
        runtime.persist_current_revision(initial_revision)?;
        Ok(runtime)
    }

    pub fn track(
        &self,
        pid: u32,
        domain: &str,
        seed: bool,
        peer_uid: u32,
    ) -> Result<TrackResult, RuntimeError> {
        let start_time = procfs::start_time(pid)?;
        let owner_uid = procfs::owner_uid(pid)?;
        if peer_uid != 0 && peer_uid != owner_uid {
            return Err(RuntimeError::UnauthorizedPid {
                peer_uid,
                pid,
                owner_uid,
            });
        }
        let domain_name = if domain.is_empty() { "default" } else { domain };
        let (group, slot, version) = self.resolve_domain(domain_name)?;
        self.track_resolved_with_start(
            pid,
            domain_name,
            &group,
            slot,
            version,
            seed,
            owner_uid,
            start_time,
            true,
        )
    }

    pub fn register_self(
        &self,
        pid: u32,
        scope: &str,
        group: &str,
        peer_uid: u32,
    ) -> Result<TrackResult, RuntimeError> {
        let start_time = procfs::start_time(pid)?;
        let owner_uid = procfs::owner_uid(pid)?;
        if owner_uid != peer_uid {
            return Err(RuntimeError::UnauthorizedPid {
                peer_uid,
                pid,
                owner_uid,
            });
        }
        let scope_name = if scope.is_empty() { "default" } else { scope };
        let (slot, version) = self.resolve_group(group)?;
        self.track_resolved_with_start(
            pid, scope_name, group, slot, version, false, owner_uid, start_time, false,
        )
    }

    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    pub fn hooks_healthy(&self) -> bool {
        self.hooks_healthy
    }

    pub fn is_dsh_group_allowed(&self, group: &str) -> bool {
        self.dsh_self_groups.contains(group)
    }

    /// DSH self-serve attach: the connecting process (pid from SO_PEERCRED) becomes the
    /// tracking root of a freshly named domain bound to a whitelisted policy group.
    pub fn attach_self(
        &self,
        pid: u32,
        policy_group: &str,
        instance_hint: Option<&str>,
        seed: bool,
        peer_uid: u32,
    ) -> Result<TrackResult, RuntimeError> {
        if policy_group.is_empty() || !self.dsh_self_groups.contains(policy_group) {
            return Err(RuntimeError::GroupNotAllowed(policy_group.to_owned()));
        }
        let start_time = procfs::start_time(pid)?;
        let owner_uid = procfs::owner_uid(pid)?;
        if owner_uid != peer_uid {
            return Err(RuntimeError::UnauthorizedPid {
                peer_uid,
                pid,
                owner_uid,
            });
        }
        let domain_name = dsh_domain_name(owner_uid, pid, start_time, instance_hint)?;
        let (slot, version) = self.resolve_group(policy_group)?;
        self.track_resolved_with_start(
            pid,
            &domain_name,
            policy_group,
            slot,
            version,
            seed,
            owner_uid,
            start_time,
            false,
        )
    }

    /// Heartbeat query: the domain the peer pid roots, if any.
    pub fn status_self(&self, pid: u32) -> Result<Option<DomainInfo>, RuntimeError> {
        let state = self.lock_state()?;
        let version = self.read_policy()?.version;
        let Some(root) = state.roots.get(&pid) else {
            return Ok(None);
        };
        Ok(state
            .domains
            .get(&root.domain)
            .map(|domain| domain_info(domain, version)))
    }

    /// Self-serve tree query: only the domain the peer pid roots.
    pub fn tree_self(&self, pid: u32) -> Result<Option<DomainTree>, RuntimeError> {
        let (name, id, roots) = {
            let state = self.lock_state()?;
            let Some(root) = state.roots.get(&pid) else {
                return Ok(None);
            };
            let Some(domain) = state.domains.get(&root.domain) else {
                return Ok(None);
            };
            (domain.name.clone(), domain.id, domain.roots.clone())
        };
        Ok(Some(self.build_tree(&name, id, &roots)?))
    }

    #[allow(clippy::too_many_arguments)]
    fn track_resolved_with_start(
        &self,
        pid: u32,
        scope_name: &str,
        group: &str,
        slot: u32,
        version: u32,
        seed: bool,
        owner_uid: u32,
        start_time: u64,
        policy_managed: bool,
    ) -> Result<TrackResult, RuntimeError> {
        let (scope_id, info) = {
            let mut state = self.lock_state()?;
            if state.roots.contains_key(&pid) {
                return Err(RuntimeError::DuplicateRoot(pid));
            }
            let existing_scope_id = state.domains.get(scope_name).map(|scope| scope.id);
            let scope_id = existing_scope_id.unwrap_or(state.next_domain_id);
            self.kernel.set_scope_policy(scope_id, slot, version)?;
            if let Err(error) = self
                .kernel
                .track_pid(pid, TrackValue { scope_id }, start_time)
            {
                if existing_scope_id.is_none() {
                    let _ = self.kernel.remove_scope_policy(scope_id);
                }
                return Err(error.into());
            }
            let committed_scope_id =
                ensure_domain(&mut state, scope_name, group, slot, policy_managed);
            debug_assert_eq!(committed_scope_id, scope_id);
            state.roots.insert(
                pid,
                RootState {
                    domain: scope_name.to_owned(),
                    start_time,
                    owner_uid,
                },
            );
            let runtime = state
                .domains
                .get_mut(scope_name)
                .ok_or(RuntimeError::UnknownDomain(scope_name.to_owned()))?;
            runtime.roots.insert(pid);
            runtime.draining = false;
            (scope_id, domain_info(runtime, version))
        };

        let seeded = if seed {
            self.seed_descendants(pid, scope_id)?
        } else {
            0
        };
        Ok(TrackResult {
            domain: info,
            seeded,
        })
    }

    pub fn untrack(&self, pid: u32, peer_uid: u32) -> Result<(), RuntimeError> {
        let mut state = self.lock_state()?;
        let root = state
            .roots
            .get(&pid)
            .ok_or(RuntimeError::UnknownRoot(pid))?;
        if peer_uid != 0 && peer_uid != root.owner_uid {
            return Err(RuntimeError::UnauthorizedRoot { peer_uid, pid });
        }
        let root = state
            .roots
            .remove(&pid)
            .ok_or(RuntimeError::UnknownRoot(pid))?;
        self.kernel.untrack_pid(pid)?;
        if let Some(domain) = state.domains.get_mut(&root.domain) {
            domain.roots.remove(&pid);
            domain.draining = domain.roots.is_empty();
        }
        Ok(())
    }

    pub fn status(&self) -> Result<RuntimeStatus, RuntimeError> {
        let pid = self.kernel.pid_tracking_status()?;
        let switches = self.switches()?;
        let policy = self.read_policy()?;
        let state = self.lock_state()?;
        let roots = state.roots.keys().copied().collect();
        let domains = state
            .domains
            .values()
            .map(|domain| domain_info(domain, policy.version))
            .collect();
        let observation = self
            .reload_observation
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .clone();
        Ok(RuntimeStatus {
            roots,
            tracked: pid.tracked,
            pid_pending: pid.pending,
            pid_generations: pid.generations,
            pid_tracked_capacity: pid.tracked_capacity,
            pid_pending_capacity: pid.pending_capacity,
            pid_start_update_failures: pid.stats.start_update_failures,
            pid_tracked_update_failures: pid.stats.tracked_update_failures,
            pid_pending_update_failures: pid.stats.pending_update_failures,
            pid_pending_fallbacks: pid.stats.pending_fallbacks,
            scope_policy_lookup_failures: pid.stats.scope_policy_lookup_failures,
            scopes: pid.scopes,
            scope_capacity: pid.scope_capacity,
            domains,
            reload_generation: policy.reload_generation,
            version: policy.version,
            active_bank: policy.active_bank,
            enables: switches.enables,
            audits: switches.audits,
            last_reload_time_ms: observation.time_ms,
            last_reload_error: observation.error,
        })
    }

    pub fn name_by_id(&self, id: u64) -> String {
        self.lock_state()
            .ok()
            .and_then(|state| state.names_by_id.get(&id).cloned())
            .unwrap_or_default()
    }

    pub fn current_revision(&self) -> Result<u64, RuntimeError> {
        Ok(u64::from(self.read_policy()?.version))
    }

    pub fn current_policy_yaml(&self) -> Result<String, RuntimeError> {
        serde_yaml::to_string(&self.read_policy()?.policy.to_root())
            .map_err(|error| RuntimeError::RevisionStore(error.to_string()))
    }

    /// Single-group read (ui.sock delegation): the group's editable YAML fragment, the
    /// current policy version (optimistic-lock token) and the active domains bound to it.
    pub fn group_policy(&self, group: &str) -> Result<(String, u32, Vec<String>), RuntimeError> {
        let policy = self.read_policy()?;
        let definition = if group == "__base__" {
            policy.policy.baseline_definition.as_ref()
        } else {
            policy.policy.group_definitions.get(group)
        }
        .ok_or_else(|| RuntimeError::MissingGroup {
            domain: "policy".into(),
            group: group.to_owned(),
        })?;
        let yaml = serde_yaml::to_string(definition)
            .map_err(|error| RuntimeError::RevisionStore(error.to_string()))?;
        let domains = self
            .lock_state()?
            .domains
            .values()
            .filter(|domain| domain.group == group)
            .map(|domain| domain.name.clone())
            .collect();
        Ok((yaml, policy.version, domains))
    }

    pub fn check_revision(&self, expected: u64) -> Result<(), RuntimeError> {
        self.ensure_revision(expected)
    }

    pub fn evaluate_intents(
        &self,
        group: &str,
        intents: &[SecurityIntent],
    ) -> Result<Vec<IntentDecision>, RuntimeError> {
        let switches = self.switches()?;
        let policy = self.read_policy()?;
        if !group.is_empty() && policy.installed.slot_for_group(group).is_none() {
            return Err(RuntimeError::MissingGroup {
                domain: "intent".into(),
                group: group.to_owned(),
            });
        }
        Ok(censorguard_policy::evaluate_intents(
            &policy.policy,
            group,
            switches.enables,
            intents,
        ))
    }

    pub fn switches(&self) -> Result<SwitchState, RuntimeError> {
        self.switches
            .lock()
            .map(|switches| *switches)
            .map_err(|_| RuntimeError::Poisoned)
    }

    /// Update runtime switches and rewrite the kernel filter config in place. The active bank
    /// and policy version are left untouched, so a switch change is not a policy reload.
    #[allow(clippy::too_many_arguments)]
    pub fn set_switches(
        &self,
        enable_file: Option<bool>,
        enable_exec: Option<bool>,
        enable_net: Option<bool>,
        audit_file: Option<bool>,
        audit_exec: Option<bool>,
        audit_net: Option<bool>,
    ) -> Result<SwitchState, RuntimeError> {
        let _reload = self
            .reload_lock
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?;
        let (bank, version) = {
            let policy = self.read_policy()?;
            (policy.active_bank, policy.version)
        };
        let mut switches = self.switches.lock().map_err(|_| RuntimeError::Poisoned)?;
        let mut next = *switches;
        if let Some(value) = enable_file {
            next.enables[0] = value;
        }
        if let Some(value) = enable_exec {
            next.enables[1] = value;
        }
        if let Some(value) = enable_net {
            next.enables[2] = value;
        }
        if let Some(value) = audit_file {
            next.audits[0] = value;
        }
        if let Some(value) = audit_exec {
            next.audits[1] = value;
        }
        if let Some(value) = audit_net {
            next.audits[2] = value;
        }
        self.kernel
            .activate_policy(next.enables, next.audits, bank, version)?;
        *switches = next;
        Ok(next)
    }

    pub fn rebind_scope(&self, scope: &str, group: &str) -> Result<DomainInfo, RuntimeError> {
        let (slot, version) = self.resolve_group(group)?;
        let mut state = self.lock_state()?;
        let runtime = state
            .domains
            .get_mut(scope)
            .ok_or_else(|| RuntimeError::UnknownDomain(scope.to_owned()))?;
        self.kernel.set_scope_policy(runtime.id, slot, version)?;
        runtime.group = group.to_owned();
        runtime.slot = slot;
        Ok(domain_info(runtime, version))
    }

    pub fn validate_policy_yaml(&self, yaml: &str) -> Result<ReloadResult, RuntimeError> {
        let canonical = compile_yaml(yaml.as_bytes())?;
        let (next_policy, _, _) = self.resolve_dns(canonical)?;
        self.apply_policy_locked(next_policy, true)
    }

    pub fn apply_policy_yaml(
        &self,
        yaml: &str,
        expected_revision: u64,
        idempotency_key: Option<&str>,
    ) -> Result<ReloadResult, RuntimeError> {
        let _reload = self
            .reload_lock
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?;
        if let Some(key) = idempotency_key
            && let Some(revision) = self
                .revisions
                .lock()
                .map_err(|_| RuntimeError::Poisoned)?
                .idempotency
                .get(key)
                .copied()
        {
            let policy = self.read_policy()?;
            let switches = self.switches()?;
            return Ok(ReloadResult {
                generation: policy.reload_generation,
                version: u32::try_from(revision).unwrap_or(u32::MAX),
                active_bank: policy.active_bank,
                changed: Vec::new(),
                enables: switches.enables,
                audits: switches.audits,
            });
        }
        self.ensure_revision(expected_revision)?;
        let canonical = compile_yaml(yaml.as_bytes())?;
        let (next_policy, cache, status) = self.resolve_dns(canonical)?;
        let result = self.apply_policy_locked(next_policy, false)?;
        {
            let mut dns = self.dns_state.lock().map_err(|_| RuntimeError::Poisoned)?;
            dns.cache = cache;
            dns.status = status;
        }
        if let Some(key) = idempotency_key {
            self.revisions
                .lock()
                .map_err(|_| RuntimeError::Poisoned)?
                .idempotency
                .insert(key.to_owned(), u64::from(result.version));
        }
        Ok(result)
    }

    pub fn rollback_policy(
        &self,
        revision: u64,
        expected_revision: u64,
        idempotency_key: Option<&str>,
    ) -> Result<ReloadResult, RuntimeError> {
        let _reload = self
            .reload_lock
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?;
        self.ensure_revision(expected_revision)?;
        let snapshot = self
            .revisions
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .snapshots
            .get(&revision)
            .cloned()
            .map_or_else(|| self.load_revision_snapshot(revision), Ok)?;
        let (snapshot, cache, status) = self.resolve_dns(snapshot)?;
        let result = self.apply_policy_locked(snapshot, false)?;
        {
            let mut dns = self.dns_state.lock().map_err(|_| RuntimeError::Poisoned)?;
            dns.cache = cache;
            dns.status = status;
        }
        if let Some(key) = idempotency_key {
            self.revisions
                .lock()
                .map_err(|_| RuntimeError::Poisoned)?
                .idempotency
                .insert(key.to_owned(), u64::from(result.version));
        }
        Ok(result)
    }

    fn ensure_revision(&self, expected: u64) -> Result<(), RuntimeError> {
        let current = self.current_revision()?;
        if current == expected {
            Ok(())
        } else {
            Err(RuntimeError::RevisionConflict { expected, current })
        }
    }

    fn persist_revision_snapshot(
        &self,
        revision: u32,
        policy: &CompiledPolicy,
    ) -> Result<(), RuntimeError> {
        let yaml = serde_yaml::to_string(&policy.to_root())
            .map_err(|error| RuntimeError::RevisionStore(error.to_string()))?;
        self.atomic_write(&self.revision_path(u64::from(revision)), yaml.as_bytes())
    }

    fn persist_current_revision(&self, revision: u32) -> Result<(), RuntimeError> {
        self.atomic_write(
            &self.revision_dir.join("current"),
            format!("{revision}\n").as_bytes(),
        )
    }

    fn load_revision_snapshot(&self, revision: u64) -> Result<CompiledPolicy, RuntimeError> {
        let path = self.revision_path(revision);
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                RuntimeError::UnknownRevision(revision)
            } else {
                RuntimeError::RevisionStore(format!("read {}: {error}", path.display()))
            }
        })?;
        compile_yaml(&bytes).map_err(RuntimeError::from)
    }

    fn revision_path(&self, revision: u64) -> PathBuf {
        self.revision_dir
            .join("revisions")
            .join(format!("{revision}.yaml"))
    }

    fn atomic_write(&self, target: &Path, bytes: &[u8]) -> Result<(), RuntimeError> {
        let parent = target.parent().ok_or_else(|| {
            RuntimeError::RevisionStore(format!("{} has no parent directory", target.display()))
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            RuntimeError::RevisionStore(format!("create {}: {error}", parent.display()))
        })?;
        let temporary = target.with_extension(format!("tmp-{}", std::process::id()));
        let result = (|| -> Result<(), RuntimeError> {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| {
                    RuntimeError::RevisionStore(format!("open {}: {error}", temporary.display()))
                })?;
            file.write_all(bytes).map_err(|error| {
                RuntimeError::RevisionStore(format!("write {}: {error}", temporary.display()))
            })?;
            file.sync_all().map_err(|error| {
                RuntimeError::RevisionStore(format!("sync {}: {error}", temporary.display()))
            })?;
            fs::rename(&temporary, target).map_err(|error| {
                RuntimeError::RevisionStore(format!(
                    "rename {} to {}: {error}",
                    temporary.display(),
                    target.display()
                ))
            })?;
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    RuntimeError::RevisionStore(format!("sync {}: {error}", parent.display()))
                })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn policy_dump(&self) -> Result<PolicyDump, RuntimeError> {
        let switches = self.switches()?;
        let policy = self.read_policy()?;
        let state = self.lock_state()?;
        let mut groups = Vec::new();
        if let Some(definition) = &policy.policy.baseline_definition {
            groups.push(GroupDump {
                name: "__base__".into(),
                version: policy.version,
                domains: state.domains.keys().cloned().collect(),
                definition: policy_definition(definition),
            });
        }
        for (name, definition) in &policy.policy.group_definitions {
            let domains = policy
                .policy
                .domains
                .iter()
                .filter(|(_, group)| *group == name)
                .map(|(domain, _)| domain.clone())
                .collect();
            groups.push(GroupDump {
                name: name.clone(),
                version: policy.version,
                domains,
                definition: policy_definition(definition),
            });
        }
        let dns = self
            .dns_state
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .status
            .iter()
            .map(|entry| DnsDomainInfo {
                domain: entry.domain.clone(),
                addresses: entry.addresses.iter().map(ToString::to_string).collect(),
                stale: entry.stale,
                error: entry.error.clone(),
            })
            .collect();
        Ok((switches, groups, dns))
    }

    pub fn trees(&self) -> Result<Vec<DomainTree>, RuntimeError> {
        let domains: Vec<_> = self
            .lock_state()?
            .domains
            .values()
            .map(|domain| (domain.name.clone(), domain.id, domain.roots.clone()))
            .collect();
        let mut result = Vec::new();
        for (name, id, roots) in domains {
            result.push(self.build_tree(&name, id, &roots)?);
        }
        Ok(result)
    }

    fn build_tree(
        &self,
        name: &str,
        id: u64,
        roots: &BTreeSet<u32>,
    ) -> Result<DomainTree, RuntimeError> {
        let mut pids = BTreeSet::new();
        for root in roots {
            pids.insert(*root);
            pids.extend(procfs::descendants(*root)?);
        }
        pids.extend(self.kernel.scoped_pids_scope(id)?);
        let mut nodes = Vec::new();
        for pid in pids {
            match procfs::process_info(pid) {
                Ok(info) => nodes.push(TreeNode {
                    pid,
                    ppid: info.ppid,
                    comm: info.comm,
                    root: roots.contains(&pid),
                }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(DomainTree {
            domain: name.to_owned(),
            domain_id: id,
            nodes,
        })
    }

    pub fn reload(
        &self,
        policy_file: Option<&str>,
        policy_yaml: Option<&str>,
        group: Option<&str>,
        dry_run: bool,
    ) -> Result<ReloadResult, RuntimeError> {
        let result = self.reload_inner(policy_file, policy_yaml, group, dry_run);
        self.observe_reload(&result);
        result
    }

    fn reload_inner(
        &self,
        policy_file: Option<&str>,
        policy_yaml: Option<&str>,
        group: Option<&str>,
        dry_run: bool,
    ) -> Result<ReloadResult, RuntimeError> {
        let _reload = self
            .reload_lock
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?;
        let canonical = if let Some(group) = group.filter(|name| !name.is_empty()) {
            let yaml = policy_yaml.ok_or(RuntimeError::MissingGroupYaml)?;
            self.read_policy()?
                .policy
                .replace_group_yaml(group, yaml.as_bytes())?
        } else if let Some(yaml) = policy_yaml.filter(|yaml| !yaml.is_empty()) {
            compile_yaml(yaml.as_bytes())?
        } else {
            let path = policy_file
                .filter(|path| !path.is_empty())
                .map(Path::new)
                .unwrap_or(&self.config_path);
            load(path)?
        };

        let (next_policy, cache, status) = self.resolve_dns(canonical)?;
        let result = self.apply_policy_locked(next_policy, dry_run)?;
        if !dry_run {
            let mut dns = self.dns_state.lock().map_err(|_| RuntimeError::Poisoned)?;
            dns.cache = cache;
            dns.status = status;
        }
        Ok(result)
    }

    pub fn bind_domain(
        &self,
        domain: &str,
        group: &str,
        dry_run: bool,
    ) -> Result<ReloadResult, RuntimeError> {
        let result = self.bind_domain_inner(domain, group, dry_run);
        self.observe_reload(&result);
        result
    }

    fn bind_domain_inner(
        &self,
        domain: &str,
        group: &str,
        dry_run: bool,
    ) -> Result<ReloadResult, RuntimeError> {
        let _reload = self
            .reload_lock
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?;
        let mut root = self.read_policy()?.policy.to_root();
        if !group.is_empty() && !root.groups.contains_key(group) {
            return Err(RuntimeError::MissingGroup {
                domain: domain.to_owned(),
                group: group.to_owned(),
            });
        }
        if let Some(domain_ref) = root.domains.iter_mut().find(|item| item.name == domain) {
            domain_ref.group = group.to_owned();
        } else if root.rules.is_empty() {
            return Err(RuntimeError::UnknownDomain(domain.to_owned()));
        }
        let (next_policy, cache, status) = self.resolve_dns(compile(root)?)?;
        let result = self.apply_policy_locked(next_policy, dry_run)?;
        if !dry_run {
            let mut dns = self.dns_state.lock().map_err(|_| RuntimeError::Poisoned)?;
            dns.cache = cache;
            dns.status = status;
        }
        Ok(result)
    }

    pub fn refresh_dns(&self) -> Result<Option<ReloadResult>, RuntimeError> {
        let result = self.refresh_dns_inner();
        match &result {
            Ok(Some(_)) | Err(_) => self.observe_reload(&result),
            Ok(None) => {}
        }
        result
    }

    fn refresh_dns_inner(&self) -> Result<Option<ReloadResult>, RuntimeError> {
        let _reload = self
            .reload_lock
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?;
        let canonical = compile(self.read_policy()?.policy.to_root())?;
        let (resolved, cache, status) = self.resolve_dns(canonical)?;
        let unchanged = {
            let current = self.read_policy()?;
            current.policy.baseline == resolved.baseline && current.policy.groups == resolved.groups
        };
        {
            let mut dns = self.dns_state.lock().map_err(|_| RuntimeError::Poisoned)?;
            dns.cache = cache;
            dns.status = status;
        }
        if unchanged {
            Ok(None)
        } else {
            self.apply_policy_locked(resolved, false).map(Some)
        }
    }

    fn apply_policy_locked(
        &self,
        next_policy: CompiledPolicy,
        dry_run: bool,
    ) -> Result<ReloadResult, RuntimeError> {
        let (
            next_layout,
            next_bank,
            next_version,
            generation,
            changed,
            previous_bank,
            previous_version,
        ) = {
            let current = self.read_policy()?;
            self.validate_reload(&current.policy, &next_policy)?;
            let layout = current.installed.evolve(&next_policy)?;
            let changed = changed_groups(&current.policy, &next_policy);
            let next_version = current.version.checked_add(1).ok_or_else(|| {
                RuntimeError::RevisionStore("u32 policy version space is exhausted".into())
            })?;
            (
                layout,
                current.active_bank ^ 1,
                next_version,
                current.reload_generation.saturating_add(1),
                changed,
                current.active_bank,
                current.version,
            )
        };
        let switches = self.switches()?;
        if dry_run {
            return Ok(ReloadResult {
                generation: self.read_policy()?.reload_generation,
                version: self.read_policy()?.version,
                active_bank: self.read_policy()?.active_bank,
                changed,
                enables: switches.enables,
                audits: switches.audits,
            });
        }

        let revision_snapshot = next_policy.clone();

        self.kernel
            .prepare_policy_bank(&next_policy, &next_layout, next_bank, next_version)?;
        self.persist_revision_snapshot(next_version, &revision_snapshot)?;
        if let Err(error) =
            self.kernel
                .activate_policy(switches.enables, switches.audits, next_bank, next_version)
        {
            let _ = fs::remove_file(self.revision_path(u64::from(next_version)));
            return Err(error.into());
        }
        if let Err(error) = self.persist_current_revision(next_version) {
            let rollback = self.kernel.activate_policy(
                switches.enables,
                switches.audits,
                previous_bank,
                previous_version,
            );
            let _ = fs::remove_file(self.revision_path(u64::from(next_version)));
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(RuntimeError::RevisionStore(format!(
                    "{error}; kernel rollback also failed: {rollback_error}"
                ))),
            };
        }

        {
            let mut policy = self.write_policy()?;
            policy.policy = Arc::new(next_policy);
            policy.installed = next_layout;
            policy.active_bank = next_bank;
            policy.version = next_version;
            policy.reload_generation = generation;
        }
        self.refresh_domain_metadata()?;
        self.revisions
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .snapshots
            .insert(u64::from(next_version), revision_snapshot);
        Ok(ReloadResult {
            generation,
            version: next_version,
            active_bank: next_bank,
            changed,
            enables: switches.enables,
            audits: switches.audits,
        })
    }

    pub fn reconcile(&self) -> Result<(), RuntimeError> {
        let roots: Vec<_> = {
            let state = self.lock_state()?;
            state
                .roots
                .iter()
                .filter_map(|(pid, root)| {
                    state.domains.get(&root.domain).map(|scope| {
                        (
                            *pid,
                            root.domain.clone(),
                            root.start_time,
                            scope.policy_managed,
                            scope.group.clone(),
                        )
                    })
                })
                .collect()
        };
        for (pid, domain_name, expected_start, policy_managed, current_group) in roots {
            let current_start = match procfs::start_time(pid) {
                Ok(value) => value,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    self.remove_dead_root(pid)?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if current_start != expected_start {
                self.remove_dead_root(pid)?;
                continue;
            }
            let (group, slot, version) = if policy_managed {
                self.resolve_domain(&domain_name)?
            } else {
                let (slot, version) = self.resolve_group(&current_group)?;
                (current_group, slot, version)
            };
            let scope_id = {
                let mut state = self.lock_state()?;
                ensure_domain(&mut state, &domain_name, &group, slot, policy_managed)
            };
            self.kernel.set_scope_policy(scope_id, slot, version)?;
            let _ = self.seed_descendants(pid, scope_id)?;
        }
        self.finalize_draining()?;
        Ok(())
    }

    fn finalize_draining(&self) -> Result<(), RuntimeError> {
        let candidates: Vec<_> = {
            let state = self.lock_state()?;
            state
                .domains
                .values()
                .filter(|domain| domain.draining && domain.roots.is_empty())
                .map(|domain| (domain.name.clone(), domain.id))
                .collect()
        };
        for (name, id) in candidates {
            if self.kernel.scoped_pids_scope(id)?.is_empty() {
                let mut state = self.lock_state()?;
                if state
                    .domains
                    .get(&name)
                    .is_some_and(|domain| domain.draining && domain.roots.is_empty())
                {
                    self.kernel.remove_scope_policy(id)?;
                    state.domains.remove(&name);
                    state.names_by_id.remove(&id);
                }
            }
        }
        Ok(())
    }

    fn validate_reload(
        &self,
        current: &CompiledPolicy,
        next: &CompiledPolicy,
    ) -> Result<(), RuntimeError> {
        let state = self.lock_state()?;
        let has_policy_roots = state
            .domains
            .values()
            .any(|domain| domain.policy_managed && !domain.roots.is_empty());
        if has_policy_roots && current.has_explicit_domains != next.has_explicit_domains {
            return Err(RuntimeError::ActiveDomainSchemaChange);
        }
        if next.has_explicit_domains {
            for domain in state
                .domains
                .values()
                .filter(|domain| domain.policy_managed && !domain.roots.is_empty())
            {
                if !next.domains.contains_key(&domain.name) {
                    return Err(RuntimeError::ActiveDomainRemoved(domain.name.clone()));
                }
            }
        }
        for scope in state
            .domains
            .values()
            .filter(|scope| !scope.policy_managed && !scope.roots.is_empty())
        {
            if !scope.group.is_empty() && !next.groups.contains_key(&scope.group) {
                return Err(RuntimeError::ActiveGroupRemoved {
                    scope: scope.name.clone(),
                    group: scope.group.clone(),
                });
            }
        }
        Ok(())
    }

    fn resolve_dns(
        &self,
        policy: CompiledPolicy,
    ) -> Result<(CompiledPolicy, DnsCache, Vec<DnsResolution>), RuntimeError> {
        let cache = self
            .dns_state
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .cache
            .clone();
        Ok(policy.resolve_dns(&cache))
    }

    fn refresh_domain_metadata(&self) -> Result<(), RuntimeError> {
        let policy = self.read_policy()?;
        let mut state = self.lock_state()?;
        for domain in state.domains.values_mut() {
            let group = if !domain.policy_managed {
                domain.group.clone()
            } else if policy.policy.has_explicit_domains {
                policy
                    .policy
                    .domains
                    .get(&domain.name)
                    .cloned()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if let Some(slot) = policy.installed.slot_for_group(&group) {
                self.kernel
                    .set_scope_policy(domain.id, slot, policy.version)?;
                domain.group = group;
                domain.slot = slot;
            }
        }
        Ok(())
    }

    fn seed_descendants(&self, root: u32, scope_id: u64) -> Result<usize, RuntimeError> {
        let mut seeded = 0;
        for pid in procfs::descendants(root)? {
            let start_time = match procfs::start_time(pid) {
                Ok(value) => value,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            self.kernel
                .track_descendant(pid, TrackValue { scope_id }, start_time)?;
            seeded += 1;
        }
        Ok(seeded)
    }

    fn remove_dead_root(&self, pid: u32) -> Result<(), RuntimeError> {
        let mut state = self.lock_state()?;
        let Some(root) = state.roots.remove(&pid) else {
            return Ok(());
        };
        self.kernel.untrack_pid(pid)?;
        if let Some(domain) = state.domains.get_mut(&root.domain) {
            domain.roots.remove(&pid);
            domain.draining = domain.roots.is_empty();
        }
        Ok(())
    }

    fn resolve_domain(&self, domain: &str) -> Result<(String, u32, u32), RuntimeError> {
        let policy = self.read_policy()?;
        let group = if policy.policy.has_explicit_domains {
            policy
                .policy
                .domains
                .get(domain)
                .ok_or_else(|| RuntimeError::UnknownDomain(domain.to_owned()))?
                .clone()
        } else {
            String::new()
        };
        let slot =
            policy
                .installed
                .slot_for_group(&group)
                .ok_or_else(|| RuntimeError::MissingGroup {
                    domain: domain.to_owned(),
                    group: group.clone(),
                })?;
        Ok((group, slot, policy.version))
    }

    fn resolve_group(&self, group: &str) -> Result<(u32, u32), RuntimeError> {
        let policy = self.read_policy()?;
        let slot =
            policy
                .installed
                .slot_for_group(group)
                .ok_or_else(|| RuntimeError::MissingGroup {
                    domain: "runtime scope".into(),
                    group: group.to_owned(),
                })?;
        Ok((slot, policy.version))
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>, RuntimeError> {
        self.state.lock().map_err(|_| RuntimeError::Poisoned)
    }

    fn read_policy(&self) -> Result<RwLockReadGuard<'_, PolicyState>, RuntimeError> {
        self.policy_state.read().map_err(|_| RuntimeError::Poisoned)
    }

    fn write_policy(&self) -> Result<RwLockWriteGuard<'_, PolicyState>, RuntimeError> {
        self.policy_state
            .write()
            .map_err(|_| RuntimeError::Poisoned)
    }

    fn observe_reload<T>(&self, result: &Result<T, RuntimeError>) {
        if let Ok(mut observation) = self.reload_observation.lock() {
            observation.time_ms = Some(unix_time_ms());
            observation.error = result.as_ref().err().map(ToString::to_string);
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn ensure_domain(
    state: &mut State,
    name: &str,
    group: &str,
    slot: u32,
    policy_managed: bool,
) -> u64 {
    if let Some(domain) = state.domains.get(name) {
        return domain.id;
    }
    let id = state.next_domain_id;
    state.next_domain_id = state.next_domain_id.saturating_add(1);
    state.names_by_id.insert(id, name.to_owned());
    state.domains.insert(
        name.to_owned(),
        DomainState {
            id,
            name: name.to_owned(),
            group: group.to_owned(),
            slot,
            roots: BTreeSet::new(),
            draining: false,
            policy_managed,
        },
    );
    id
}

fn domain_info(domain: &DomainState, version: u32) -> DomainInfo {
    DomainInfo {
        name: domain.name.clone(),
        id: domain.id,
        slot: domain.slot,
        group: domain.group.clone(),
        roots: domain.roots.len(),
        version,
        draining: domain.draining,
    }
}

/// `dsh-<uid>-<tgid>-<starttime>[-hint]`: the start time makes the name unique across
/// pid reuse; the optional hint distinguishes multiple instances of the same user.
fn dsh_domain_name(
    uid: u32,
    tgid: u32,
    start_time: u64,
    hint: Option<&str>,
) -> Result<String, RuntimeError> {
    let mut name = format!("dsh-{uid}-{tgid}-{start_time}");
    if let Some(hint) = hint.filter(|hint| !hint.is_empty()) {
        let valid = hint.len() <= 32
            && hint
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.');
        if !valid {
            return Err(RuntimeError::InvalidInstanceHint(hint.to_owned()));
        }
        name.push('-');
        name.push_str(hint);
    }
    Ok(name)
}

fn changed_groups(current: &CompiledPolicy, next: &CompiledPolicy) -> Vec<String> {
    let mut names: BTreeSet<_> = current
        .groups
        .keys()
        .chain(next.groups.keys())
        .cloned()
        .collect();
    names.insert("__base__".into());
    names
        .into_iter()
        .filter(|name| {
            if name == "__base__" {
                current.baseline != next.baseline
            } else {
                current.groups.get(name) != next.groups.get(name)
            }
        })
        .collect()
}

fn policy_definition(group: &GroupPolicy) -> PolicyDefinition {
    PolicyDefinition {
        rules: group.rules.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::dsh_domain_name;

    #[test]
    fn dsh_domain_name_embeds_start_time_and_optional_hint() -> Result<(), super::RuntimeError> {
        assert_eq!(
            dsh_domain_name(1000, 4242, 987_654, None)?,
            "dsh-1000-4242-987654"
        );
        assert_eq!(
            dsh_domain_name(1000, 4242, 987_654, Some("web-1"))?,
            "dsh-1000-4242-987654-web-1"
        );
        Ok(())
    }

    #[test]
    fn dsh_domain_name_rejects_unsafe_hints() {
        assert!(dsh_domain_name(1000, 1, 1, Some("bad hint")).is_err());
        assert!(dsh_domain_name(1000, 1, 1, Some("bad/hint")).is_err());
        assert!(dsh_domain_name(1000, 1, 1, Some(&"x".repeat(33))).is_err());
        assert!(dsh_domain_name(1000, 1, 1, Some("ok_hint.v2")).is_ok());
        assert!(dsh_domain_name(1000, 1, 1, Some("")).is_ok());
    }
}
