//! Safe Rust wrapper around the small C/libbpf loading boundary.

mod ffi;

use censorguard_common::abi::{
    ArgKey, FilterConfig, InodeKey, InodeRuleValue, Net6LpmKey, Net6PortLpmKey, NetLpmKey,
    NetPortLpmKey, NetRuleValue, PID_PENDING_CAPACITY, PID_TRACKED_CAPACITY, PathKey,
    PendingProcessOp, PidTrackingStats, RawEvent, SCOPE_POLICY_CAPACITY, ScopePolicyValue,
    SlotMeta, StringRuleValue, TrackValue,
};
use censorguard_policy::{CompiledPolicy, CompiledRules, RuleValue};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::{CStr, CString, c_void};
use std::fmt;
use std::mem::size_of;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Feature {
    ProcessTree,
    File,
    Exec,
    Network,
    SelfProtection,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProgramSpec {
    pub name: &'static str,
    pub section: &'static str,
    pub feature: Feature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookManifest {
    required: Vec<ProgramSpec>,
}

impl HookManifest {
    #[must_use]
    pub fn for_policy(enables: [bool; 3]) -> Self {
        let mut required = PROCESS_TREE.to_vec();
        required.extend_from_slice(SELF_PROTECTION);
        if enables[0] {
            required.extend_from_slice(FILE);
        }
        if enables[1] {
            required.extend_from_slice(EXEC);
        }
        if enables[2] {
            required.extend_from_slice(NETWORK);
        }
        Self { required }
    }

    #[must_use]
    pub fn required(&self) -> &[ProgramSpec] {
        &self.required
    }

    pub fn validate(&self, attached: &BTreeSet<String>) -> Result<(), KernelError> {
        let missing: Vec<_> = self
            .required
            .iter()
            .filter(|program| !attached.contains(program.name))
            .map(|program| program.name)
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(KernelError::MissingHooks(missing))
        }
    }
}

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("BPF object does not exist: {0}")]
    MissingObject(PathBuf),
    #[error("required BPF hooks were not attached: {0:?}")]
    MissingHooks(Vec<&'static str>),
    #[error("value contains an embedded NUL byte: {0}")]
    EmbeddedNul(String),
    #[error("libbpf: {0}")]
    Native(String),
    #[error("BPF map {0:?} was not found")]
    MissingMap(String),
    #[error("policy requires {actual} policy group slots; maximum is {maximum}")]
    TooManyPolicyGroups { actual: usize, maximum: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledPolicy {
    pub group_slots: BTreeMap<String, u32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PidTrackingStatus {
    pub tracked: usize,
    pub pending: usize,
    pub generations: usize,
    pub tracked_capacity: usize,
    pub pending_capacity: usize,
    pub scopes: usize,
    pub scope_capacity: usize,
    pub stats: PidTrackingStats,
}

impl InstalledPolicy {
    pub fn initial(policy: &CompiledPolicy) -> Result<Self, KernelError> {
        Self {
            group_slots: BTreeMap::new(),
        }
        .evolve(policy)
    }

    pub fn evolve(&self, policy: &CompiledPolicy) -> Result<Self, KernelError> {
        let required = required_policy_groups(policy);
        if required.len() > 63 {
            return Err(KernelError::TooManyPolicyGroups {
                actual: required.len(),
                maximum: 63,
            });
        }

        // Release deleted groups before allocating additions. This permits an atomic 63 → 63
        // policy replacement without reporting a false capacity overflow.
        let mut group_slots = self.group_slots.clone();
        group_slots.retain(|group, _| required.contains(group));
        let mut used: BTreeSet<u32> = group_slots.values().copied().collect();
        for group in required {
            if group_slots.contains_key(&group) {
                continue;
            }
            let slot = (1..64).find(|slot| !used.contains(slot)).ok_or(
                KernelError::TooManyPolicyGroups {
                    actual: policy.groups.len(),
                    maximum: 63,
                },
            )?;
            used.insert(slot);
            group_slots.insert(group, slot);
        }
        Ok(Self { group_slots })
    }

    #[must_use]
    pub fn slot_for_group(&self, group: &str) -> Option<u32> {
        self.group_slots.get(group).copied()
    }
}

fn required_policy_groups(policy: &CompiledPolicy) -> BTreeSet<String> {
    let mut groups: BTreeSet<String> = policy.groups.keys().cloned().collect();
    if !policy.has_explicit_domains || policy.domains.values().any(String::is_empty) {
        groups.insert(String::new());
    }
    groups
}

pub struct NativeKernel {
    handle: NonNull<ffi::KernelHandle>,
}

// SAFETY: after construction the C session's object metadata and links are immutable. Runtime
// operations use independent BPF syscalls on map fds. Destruction occurs only after all Arc owners,
// including EventReader, are gone.
unsafe impl Send for NativeKernel {}
unsafe impl Sync for NativeKernel {}

pub struct EventReader {
    handle: NonNull<ffi::EventReaderHandle>,
    _kernel: Arc<NativeKernel>,
}

// SAFETY: an EventReader is moved to one consumer thread and all access requires `&mut self`.
unsafe impl Send for EventReader {}

impl NativeKernel {
    pub fn load(object: impl AsRef<Path>, manifest: &HookManifest) -> Result<Self, KernelError> {
        let object = object.as_ref();
        if !object.is_file() {
            return Err(KernelError::MissingObject(object.to_path_buf()));
        }
        let path = CString::new(object.as_os_str().as_bytes())
            .map_err(|_| KernelError::EmbeddedNul(object.display().to_string()))?;
        let names: Result<Vec<_>, _> = manifest
            .required()
            .iter()
            .map(|program| CString::new(program.name))
            .collect();
        let names = names.map_err(|_| KernelError::EmbeddedNul("program name".into()))?;
        let pointers: Vec<_> = names.iter().map(|name| name.as_ptr()).collect();
        let mut error = [0_i8; 4096];
        // SAFETY: every pointer is NUL-terminated and remains alive for the call. The C function
        // either returns an owned opaque handle or null and writes at most `error.len()` bytes.
        let handle = unsafe {
            ffi::as_kernel_load(
                path.as_ptr(),
                pointers.as_ptr(),
                pointers.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let handle = NonNull::new(handle).ok_or_else(|| {
            // SAFETY: the C shim always NUL-terminates the fixed error buffer when it writes it;
            // the zero-initialized buffer is also a valid empty C string if no message was set.
            let message = unsafe { CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            KernelError::Native(if message.is_empty() {
                "unknown loader failure".into()
            } else {
                message
            })
        })?;
        Ok(Self { handle })
    }

    pub fn map_fd(&self, name: &str) -> Result<RawFd, KernelError> {
        let name = CString::new(name).map_err(|_| KernelError::EmbeddedNul(name.into()))?;
        // SAFETY: `self.handle` is owned and live; `name` is a live NUL-terminated string.
        let fd = unsafe { ffi::as_kernel_map_fd(self.handle.as_ptr(), name.as_ptr()) };
        if fd < 0 {
            Err(KernelError::MissingMap(name.to_string_lossy().into_owned()))
        } else {
            Ok(fd)
        }
    }

    pub fn install_policy(&self, policy: &CompiledPolicy) -> Result<InstalledPolicy, KernelError> {
        self.install_policy_version(policy, 1, [true; 3], [false; 3])
    }

    pub fn install_policy_version(
        &self,
        policy: &CompiledPolicy,
        version: u32,
        enables: [bool; 3],
        audits: [bool; 3],
    ) -> Result<InstalledPolicy, KernelError> {
        let layout = InstalledPolicy::initial(policy)?;
        self.prepare_policy_bank(policy, &layout, 0, version)?;
        self.activate_policy(enables, audits, 0, version)?;
        Ok(layout)
    }

    pub fn prepare_policy_bank(
        &self,
        policy: &CompiledPolicy,
        layout: &InstalledPolicy,
        bank: u32,
        version: u32,
    ) -> Result<(), KernelError> {
        let empty = CompiledRules::default();
        let offset = (bank & 1) * 64;
        self.replace_rule_set(offset, policy.baseline.as_ref().unwrap_or(&empty), version)?;
        self.write_slot_meta(offset, 0, version)?;

        let mut representatives: Vec<(&CompiledRules, u32)> =
            vec![(policy.baseline.as_ref().unwrap_or(&empty), offset)];

        for (group, slot) in &layout.group_slots {
            let rules = if group.is_empty() {
                &empty
            } else {
                policy.groups.get(group).ok_or_else(|| {
                    KernelError::Native(format!("layout policy group {group:?} is missing"))
                })?
            };
            if let Some((_, source)) = representatives
                .iter()
                .find(|(candidate, _)| **candidate == *rules)
            {
                self.clone_rule_slot(*source, offset + slot)?;
            } else {
                self.replace_rule_set(offset + slot, rules, version)?;
                representatives.push((rules, offset + slot));
            }
            self.write_slot_meta(offset + slot, *slot, version)?;
        }
        Ok(())
    }

    pub fn activate_policy(
        &self,
        enables: [bool; 3],
        audits: [bool; 3],
        bank: u32,
        version: u32,
    ) -> Result<(), KernelError> {
        // The ABI keeps the legacy allow_sample_rate field: any enabled audit switch turns on
        // full ALLOW reporting so the global audit switches take effect in should_report_allow().
        self.update_map(
            "filter_config_map",
            &0_u32,
            &FilterConfig {
                enable_file: u32::from(enables[0]),
                enable_exec: u32::from(enables[1]),
                enable_net: u32::from(enables[2]),
                file_mode: 1,
                active_bank: bank & 1,
                policy_version: version,
                allow_sample_rate: u32::from(audits.iter().any(|audit| *audit)),
                audit_file: u32::from(audits[0]),
                audit_exec: u32::from(audits[1]),
                audit_net: u32::from(audits[2]),
            },
        )
    }

    fn write_slot_meta(
        &self,
        physical_slot: u32,
        logical_slot: u32,
        version: u32,
    ) -> Result<(), KernelError> {
        self.update_map(
            "slot_meta_map",
            &physical_slot,
            &SlotMeta {
                version,
                policy_group: logical_slot,
                switch_ts: unix_nanos(),
            },
        )
    }

    fn clone_rule_slot(&self, source: u32, destination: u32) -> Result<(), KernelError> {
        let mut error = [0_i8; 4096];
        // SAFETY: both slots belong to the same loaded outer maps. The C shim resolves each source
        // map ID to a live fd before assigning it to the destination outer slot.
        let result = unsafe {
            ffi::as_kernel_clone_rule_slot(
                self.handle.as_ptr(),
                source,
                destination,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        native_result(result, &error)
    }

    pub fn protect_pid(&self, pid: u32) -> Result<(), KernelError> {
        self.update_map("protected_pids", &pid, &1_u8)
    }

    pub fn set_scope_policy(
        &self,
        scope_id: u64,
        policy_slot: u32,
        generation: u32,
    ) -> Result<(), KernelError> {
        self.update_map(
            "scope_policies",
            &scope_id,
            &ScopePolicyValue {
                policy_slot,
                generation,
            },
        )
    }

    pub fn remove_scope_policy(&self, scope_id: u64) -> Result<(), KernelError> {
        self.delete_map("scope_policies", &scope_id)
    }

    pub fn track_pid(
        &self,
        pid: u32,
        value: TrackValue,
        start_boottime: u64,
    ) -> Result<(), KernelError> {
        // Generation must exist before the tracked entry becomes visible to BPF hooks. This makes
        // activation one-way: an incomplete pair is never treated as in scope.
        self.update_map("pid_start_times", &pid, &start_boottime)?;
        if let Err(error) = self.update_map("tracked_pids", &pid, &value) {
            let _ = self.delete_map("pid_start_times", &pid);
            return Err(error);
        }
        let _ = self.delete_map("pending_child_proc_ops", &pid);
        Ok(())
    }

    /// Track a discovered descendant, falling back to the generation-safe pending map when the
    /// primary pair cannot be installed. `Ok(true)` means the PID is protected by pending scope.
    pub fn track_descendant(
        &self,
        pid: u32,
        value: TrackValue,
        start_boottime: u64,
    ) -> Result<bool, KernelError> {
        let primary_error = match self.track_pid(pid, value, start_boottime) {
            Ok(()) => return Ok(false),
            Err(error) => error,
        };
        self.update_map(
            "pending_child_proc_ops",
            &pid,
            &PendingProcessOp {
                scope_id: value.scope_id,
                child_start_boottime: start_boottime,
            },
        )
        .map_err(|pending_error| {
            KernelError::Native(format!(
                "primary PID tracking failed ({primary_error}); pending fallback failed ({pending_error})"
            ))
        })?;
        Ok(true)
    }

    pub fn untrack_pid(&self, pid: u32) -> Result<(), KernelError> {
        self.delete_map("tracked_pids", &pid)?;
        self.delete_map("pid_start_times", &pid)?;
        self.delete_map("pending_child_proc_ops", &pid)
    }

    pub fn pid_tracking_status(&self) -> Result<PidTrackingStatus, KernelError> {
        Ok(PidTrackingStatus {
            tracked: self.map_count("tracked_pids")?,
            pending: self.map_count("pending_child_proc_ops")?,
            generations: self.map_count("pid_start_times")?,
            tracked_capacity: PID_TRACKED_CAPACITY,
            pending_capacity: PID_PENDING_CAPACITY,
            scopes: self.map_count("scope_policies")?,
            scope_capacity: SCOPE_POLICY_CAPACITY,
            stats: self.lookup_map("pid_track_stats", &0_u32)?,
        })
    }

    fn map_pids_scope(
        &self,
        map_name: &str,
        scope_id: u64,
        capacity: usize,
    ) -> Result<Vec<u32>, KernelError> {
        let name =
            CString::new(map_name).map_err(|_| KernelError::EmbeddedNul(map_name.to_owned()))?;
        let mut pids = vec![0_u32; capacity];
        let mut error = [0_i8; 4096];
        // SAFETY: `pids` provides writable storage for `capacity` u32 values and the kernel
        // handle remains live for the duration of the bounded map iteration.
        let count = unsafe {
            ffi::as_kernel_list_scope(
                self.handle.as_ptr(),
                name.as_ptr(),
                scope_id,
                pids.as_mut_ptr(),
                pids.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if count < 0 {
            return Err(native_error(&error));
        }
        let count = usize::try_from(count)
            .map_err(|_| KernelError::Native("scope PID count overflow".into()))?;
        pids.truncate(count);
        Ok(pids)
    }

    pub fn scoped_pids_scope(&self, scope_id: u64) -> Result<Vec<u32>, KernelError> {
        let mut pids: BTreeSet<u32> = self
            .map_pids_scope("tracked_pids", scope_id, PID_TRACKED_CAPACITY)?
            .into_iter()
            .collect();
        pids.extend(self.map_pids_scope(
            "pending_child_proc_ops",
            scope_id,
            PID_PENDING_CAPACITY,
        )?);
        Ok(pids.into_iter().collect())
    }

    pub fn event_reader(self: &Arc<Self>) -> Result<EventReader, KernelError> {
        let mut error = [0_i8; 4096];
        // SAFETY: the kernel handle is valid and retained by the Arc stored in EventReader.
        let handle = unsafe {
            ffi::as_event_reader_new(self.handle.as_ptr(), error.as_mut_ptr(), error.len())
        };
        let handle = NonNull::new(handle).ok_or_else(|| native_error(&error))?;
        Ok(EventReader {
            handle,
            _kernel: Arc::clone(self),
        })
    }

    fn replace_rule_set(
        &self,
        slot: u32,
        rules: &CompiledRules,
        version: u32,
    ) -> Result<(), KernelError> {
        let (keys, values): (Vec<_>, Vec<_>) = rules
            .file_strings
            .iter()
            .map(|(key, value)| (*key, string_value(*value, version)))
            .unzip();
        self.replace_inner("dom_file_str", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .file_inodes
            .iter()
            .map(|(key, value)| (*key, inode_value(*value, version)))
            .unzip();
        self.replace_inner("dom_file_ino", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .directory_inodes
            .iter()
            .map(|(key, value)| (*key, inode_value(*value, version)))
            .unzip();
        self.replace_inner("dom_dir_ino", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .commands
            .iter()
            .map(|(key, value)| (*key, string_value(*value, version)))
            .unzip();
        self.replace_inner("dom_cmd", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .command_inodes
            .iter()
            .map(|(key, value)| (*key, inode_value(*value, version)))
            .unzip();
        self.replace_inner("dom_cmd_ino", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .arguments
            .iter()
            .map(|(key, value)| (*key, string_value(*value, version)))
            .unzip();
        self.replace_inner("dom_arg", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .network
            .iter()
            .map(|(key, value)| (*key, network_value(*value, version)))
            .unzip();
        self.replace_inner("dom_net", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .network_ports
            .iter()
            .map(|(key, value)| (*key, network_value(*value, version)))
            .unzip();
        self.replace_inner("dom_net_port", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .network6
            .iter()
            .map(|(key, value)| (*key, network_value(*value, version)))
            .unzip();
        self.replace_inner("dom_net6", slot, &keys, &values)?;

        let (keys, values): (Vec<_>, Vec<_>) = rules
            .network6_ports
            .iter()
            .map(|(key, value)| (*key, network_value(*value, version)))
            .unzip();
        self.replace_inner("dom_net6_port", slot, &keys, &values)
    }

    fn update_map<K: KernelPod, V: KernelPod>(
        &self,
        map_name: &str,
        key: &K,
        value: &V,
    ) -> Result<(), KernelError> {
        let name =
            CString::new(map_name).map_err(|_| KernelError::EmbeddedNul(map_name.to_owned()))?;
        let mut error = [0_i8; 4096];
        // SAFETY: K/V implement the private KernelPod marker and contain initialized, pointer-free
        // bytes matching their C ABI. The C shim validates key/value sizes before the syscall.
        let result = unsafe {
            ffi::as_kernel_update_map(
                self.handle.as_ptr(),
                name.as_ptr(),
                std::ptr::from_ref(key).cast::<c_void>(),
                size_of::<K>(),
                std::ptr::from_ref(value).cast::<c_void>(),
                size_of::<V>(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        native_result(result, &error)
    }

    fn lookup_map<K: KernelPod, V: Default + KernelPod>(
        &self,
        map_name: &str,
        key: &K,
    ) -> Result<V, KernelError> {
        let name =
            CString::new(map_name).map_err(|_| KernelError::EmbeddedNul(map_name.to_owned()))?;
        let mut value = V::default();
        let mut error = [0_i8; 4096];
        // SAFETY: K/V are initialized pointer-free ABI values and the C shim validates sizes.
        let result = unsafe {
            ffi::as_kernel_lookup_map(
                self.handle.as_ptr(),
                name.as_ptr(),
                std::ptr::from_ref(key).cast::<c_void>(),
                size_of::<K>(),
                std::ptr::from_mut(&mut value).cast::<c_void>(),
                size_of::<V>(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        native_result(result, &error)?;
        Ok(value)
    }

    fn delete_map<K: KernelPod>(&self, map_name: &str, key: &K) -> Result<(), KernelError> {
        let name =
            CString::new(map_name).map_err(|_| KernelError::EmbeddedNul(map_name.to_owned()))?;
        let mut error = [0_i8; 4096];
        // SAFETY: K is a private KernelPod and the C shim validates the key size.
        let result = unsafe {
            ffi::as_kernel_delete_map(
                self.handle.as_ptr(),
                name.as_ptr(),
                std::ptr::from_ref(key).cast::<c_void>(),
                size_of::<K>(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        native_result(result, &error)
    }

    fn map_count(&self, map_name: &str) -> Result<usize, KernelError> {
        let name =
            CString::new(map_name).map_err(|_| KernelError::EmbeddedNul(map_name.to_owned()))?;
        let mut error = [0_i8; 4096];
        // SAFETY: the kernel and map name pointers are valid for the duration of the call.
        let count = unsafe {
            ffi::as_kernel_map_count(
                self.handle.as_ptr(),
                name.as_ptr(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if count < 0 {
            Err(native_error(&error))
        } else {
            usize::try_from(count).map_err(|_| KernelError::Native("map count overflow".into()))
        }
    }

    fn replace_inner<K: KernelPod, V: KernelPod>(
        &self,
        outer_name: &str,
        slot: u32,
        keys: &[K],
        values: &[V],
    ) -> Result<(), KernelError> {
        debug_assert_eq!(keys.len(), values.len());
        let name = CString::new(outer_name)
            .map_err(|_| KernelError::EmbeddedNul(outer_name.to_owned()))?;
        let mut error = [0_i8; 4096];
        // SAFETY: the slices are equally sized contiguous arrays of private KernelPod values and
        // remain alive for the call. The C shim validates ABI sizes and count before reading them.
        let result = unsafe {
            ffi::as_kernel_replace_inner(
                self.handle.as_ptr(),
                name.as_ptr(),
                slot,
                keys.as_ptr().cast::<c_void>(),
                values.as_ptr().cast::<c_void>(),
                keys.len(),
                size_of::<K>(),
                size_of::<V>(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        native_result(result, &error)
    }
}

impl EventReader {
    pub fn next(&mut self, timeout: Duration) -> Result<Option<RawEvent>, KernelError> {
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut event = RawEvent::default();
        let mut error = [0_i8; 4096];
        // SAFETY: the event reader is exclusively borrowed, RawEvent is writable and its size is
        // checked by the C shim against the ringbuf sample before success is returned.
        let result = unsafe {
            ffi::as_event_reader_next(
                self.handle.as_ptr(),
                std::ptr::from_mut(&mut event).cast::<c_void>(),
                size_of::<RawEvent>(),
                timeout_ms,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        match result {
            1 => Ok(Some(event)),
            0 => Ok(None),
            _ => Err(native_error(&error)),
        }
    }

    #[must_use]
    pub fn reader_dropped(&self) -> u64 {
        // SAFETY: the reader handle remains owned and live for this immutable counter read.
        let dropped = unsafe { ffi::as_event_reader_dropped(self.handle.as_ptr()) };
        u64::try_from(dropped).unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn kernel_dropped(&self) -> u64 {
        // SAFETY: the reader handle owns a valid fd for the BPF event_drops map.
        let dropped = unsafe { ffi::as_event_reader_kernel_dropped(self.handle.as_ptr()) };
        u64::try_from(dropped).unwrap_or(u64::MAX)
    }
}

impl fmt::Debug for EventReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventReader")
            .finish_non_exhaustive()
    }
}

impl Drop for EventReader {
    fn drop(&mut self) {
        // SAFETY: this handle was returned by as_event_reader_new and is freed once.
        unsafe { ffi::as_event_reader_free(self.handle.as_ptr()) };
    }
}

fn native_result(result: i32, error: &[i8]) -> Result<(), KernelError> {
    if result == 0 {
        return Ok(());
    }
    // SAFETY: all error arrays are zero-initialized and the C shim writes with `vsnprintf`.
    Err(native_error(error))
}

fn native_error(error: &[i8]) -> KernelError {
    // SAFETY: all error arrays are zero-initialized and the C shim writes with `vsnprintf`.
    let message = unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    KernelError::Native(if message.is_empty() {
        "unknown native error".into()
    } else {
        message
    })
}

fn unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

const fn inode_value(value: RuleValue, version: u32) -> InodeRuleValue {
    InodeRuleValue {
        mask: value.mask,
        action: value.action,
        audit: if value.audit { 1 } else { 0 },
        pad: 0,
        version,
    }
}

const fn string_value(value: RuleValue, version: u32) -> StringRuleValue {
    inode_value(value, version)
}

const fn network_value(value: RuleValue, version: u32) -> NetRuleValue {
    NetRuleValue {
        action: value.action,
        audit: if value.audit { 1 } else { 0 },
        pad: [0; 2],
        forbid_labels: 0,
        version,
    }
}

/// Marker for values that can be copied byte-for-byte across the Rust/C/BPF ABI.
///
/// # Safety
///
/// Implementors must have a stable C-compatible layout, contain no pointers or
/// references, and be fully initialized so the C shim may read every byte.
unsafe trait KernelPod: Copy {}

macro_rules! impl_kernel_pod {
    ($($type:ty),+ $(,)?) => {
        $(
            unsafe impl KernelPod for $type {}
        )+
    };
}

impl_kernel_pod!(
    u8,
    u32,
    u64,
    FilterConfig,
    PathKey,
    InodeKey,
    ArgKey,
    NetLpmKey,
    NetPortLpmKey,
    Net6LpmKey,
    Net6PortLpmKey,
    InodeRuleValue,
    NetRuleValue,
    TrackValue,
    PendingProcessOp,
    ScopePolicyValue,
    PidTrackingStats,
    SlotMeta,
);

impl fmt::Debug for NativeKernel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeKernel")
            .finish_non_exhaustive()
    }
}

impl Drop for NativeKernel {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by `as_kernel_load` and is freed exactly once here.
        unsafe { ffi::as_kernel_free(self.handle.as_ptr()) };
    }
}

const PROCESS_TREE: &[ProgramSpec] = &[
    ProgramSpec {
        name: "handle_sched_process_fork",
        section: "raw_tracepoint/sched_process_fork",
        feature: Feature::ProcessTree,
    },
    ProgramSpec {
        name: "handle_sched_process_exec",
        section: "tracepoint/sched/sched_process_exec",
        feature: Feature::ProcessTree,
    },
    ProgramSpec {
        name: "handle_sched_process_exit",
        section: "tracepoint/sched/sched_process_exit",
        feature: Feature::ProcessTree,
    },
];

const SELF_PROTECTION: &[ProgramSpec] = &[
    ProgramSpec {
        name: "enforce_task_kill",
        section: "lsm/task_kill",
        feature: Feature::SelfProtection,
    },
    ProgramSpec {
        name: "enforce_ptrace_access_check",
        section: "lsm/ptrace_access_check",
        feature: Feature::SelfProtection,
    },
    ProgramSpec {
        name: "enforce_ptrace_traceme",
        section: "lsm/ptrace_traceme",
        feature: Feature::SelfProtection,
    },
    ProgramSpec {
        name: "enforce_bpf_guard",
        section: "lsm/bpf",
        feature: Feature::SelfProtection,
    },
];

const FILE: &[ProgramSpec] = &[
    ProgramSpec {
        name: "enforce_file_open",
        section: "lsm.s/file_open",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_file_permission",
        section: "lsm/file_permission",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_truncate",
        section: "lsm/path_truncate",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_unlink",
        section: "lsm/path_unlink",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_rmdir",
        section: "lsm/path_rmdir",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_rename",
        section: "lsm/path_rename",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_file_truncate",
        section: "lsm/file_truncate",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_link",
        section: "lsm/path_link",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_symlink",
        section: "lsm/path_symlink",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_mkdir",
        section: "lsm/path_mkdir",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_chmod",
        section: "lsm/path_chmod",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_chown",
        section: "lsm/path_chown",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_inode_setxattr",
        section: "lsm/inode_setxattr",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_inode_removexattr",
        section: "lsm/inode_removexattr",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_inode_set_acl",
        section: "lsm/inode_set_acl",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_path_mknod",
        section: "lsm/path_mknod",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_mmap_file",
        section: "lsm/mmap_file",
        feature: Feature::File,
    },
    ProgramSpec {
        name: "enforce_file_mprotect",
        section: "lsm/file_mprotect",
        feature: Feature::File,
    },
];

const EXEC: &[ProgramSpec] = &[
    ProgramSpec {
        name: "enforce_bprm_check",
        section: "lsm/bprm_check_security",
        feature: Feature::Exec,
    },
    ProgramSpec {
        name: "capture_exec_args",
        section: "tracepoint/syscalls/sys_enter_execve",
        feature: Feature::Exec,
    },
    ProgramSpec {
        name: "capture_execveat_args",
        section: "tracepoint/syscalls/sys_enter_execveat",
        feature: Feature::Exec,
    },
];

const NETWORK: &[ProgramSpec] = &[ProgramSpec {
    name: "enforce_socket_connect",
    section: "lsm/socket_connect",
    feature: Feature::Network,
}];

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with_groups(groups: &[&str], domains: &[(&str, &str)]) -> CompiledPolicy {
        CompiledPolicy {
            baseline: None,
            groups: groups
                .iter()
                .map(|name| ((*name).to_owned(), CompiledRules::default()))
                .collect(),
            domains: domains
                .iter()
                .map(|(domain, group)| ((*domain).to_owned(), (*group).to_owned()))
                .collect(),
            has_explicit_domains: !domains.is_empty(),
            warnings: Vec::new(),
            baseline_definition: None,
            group_definitions: BTreeMap::new(),
        }
    }

    #[test]
    fn readiness_requires_every_enabled_hook() {
        let manifest = HookManifest::for_policy([true, false, false]);
        let mut attached: BTreeSet<_> = manifest
            .required()
            .iter()
            .map(|program| program.name.to_owned())
            .collect();
        assert!(manifest.validate(&attached).is_ok());
        attached.remove("enforce_path_unlink");
        assert!(matches!(
            manifest.validate(&attached),
            Err(KernelError::MissingHooks(_))
        ));
    }

    #[test]
    fn exec_readiness_requires_execveat_argument_capture() {
        let manifest = HookManifest::for_policy([false, true, false]);
        let mut attached: BTreeSet<_> = manifest
            .required()
            .iter()
            .map(|program| program.name.to_owned())
            .collect();
        assert!(attached.contains("capture_exec_args"));
        assert!(attached.contains("capture_execveat_args"));
        attached.remove("capture_execveat_args");
        assert!(matches!(
            manifest.validate(&attached),
            Err(KernelError::MissingHooks(missing))
                if missing == vec!["capture_execveat_args"]
        ));
    }

    #[test]
    fn many_domains_share_policy_group_slots() -> Result<(), KernelError> {
        let groups = ["read-only", "strict"];
        let domain_names: Vec<_> = (0..100).map(|index| format!("session-{index}")).collect();
        let domains: Vec<_> = domain_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.as_str(), groups[index % groups.len()]))
            .collect();
        let layout = InstalledPolicy::initial(&policy_with_groups(&groups, &domains))?;
        assert_eq!(layout.group_slots.len(), 2);
        assert_eq!(layout.slot_for_group("read-only"), Some(1));
        assert_eq!(layout.slot_for_group("strict"), Some(2));
        Ok(())
    }

    #[test]
    fn policy_group_slot_capacity_is_enforced() -> Result<(), KernelError> {
        let names: Vec<_> = (0..63).map(|index| format!("group-{index:02}")).collect();
        let refs: Vec<_> = names.iter().map(String::as_str).collect();
        let layout = InstalledPolicy::initial(&policy_with_groups(&refs, &[("session", refs[0])]))?;
        assert_eq!(layout.group_slots.len(), 63);

        let overflow_names: Vec<_> = (0..64).map(|index| format!("group-{index:02}")).collect();
        let overflow_refs: Vec<_> = overflow_names.iter().map(String::as_str).collect();
        assert!(matches!(
            InstalledPolicy::initial(&policy_with_groups(
                &overflow_refs,
                &[("session", overflow_refs[0])]
            )),
            Err(KernelError::TooManyPolicyGroups {
                actual: 64,
                maximum: 63
            })
        ));
        Ok(())
    }

    #[test]
    fn evolving_layout_preserves_and_reuses_group_slots() -> Result<(), KernelError> {
        let initial = InstalledPolicy::initial(&policy_with_groups(
            &["alpha", "beta"],
            &[("session", "alpha")],
        ))?;
        let alpha_slot = initial
            .slot_for_group("alpha")
            .ok_or_else(|| KernelError::Native("alpha slot missing in test".into()))?;
        let beta_slot = initial
            .slot_for_group("beta")
            .ok_or_else(|| KernelError::Native("beta slot missing in test".into()))?;

        let evolved = initial.evolve(&policy_with_groups(
            &["alpha", "gamma"],
            &[("session", "alpha")],
        ))?;
        assert_eq!(evolved.slot_for_group("alpha"), Some(alpha_slot));
        assert_eq!(evolved.slot_for_group("gamma"), Some(beta_slot));
        assert_eq!(evolved.slot_for_group("beta"), None);
        Ok(())
    }

    #[test]
    fn implicit_and_empty_group_domains_share_the_empty_slot() -> Result<(), KernelError> {
        let implicit = InstalledPolicy::initial(&policy_with_groups(&[], &[]))?;
        assert_eq!(implicit.slot_for_group(""), Some(1));

        let explicit = InstalledPolicy::initial(&policy_with_groups(
            &["strict"],
            &[("unrestricted", ""), ("guarded", "strict")],
        ))?;
        assert_eq!(explicit.group_slots.len(), 2);
        assert!(explicit.slot_for_group("").is_some());
        Ok(())
    }
}
