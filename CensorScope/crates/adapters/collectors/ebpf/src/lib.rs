//! eBPF collector for process fork, exec, and exit lifecycle events.
// Harness shell calls propagate `DSH_CENSORSCOPE_CALL_ID` and the CensorScope trace
// session identity; the decoded session identity is retained in each raw
// envelope and call IDs are persisted when supplied by collectors that expose
// the optional ABI field.

pub mod capability_probe;
mod path_state;
pub mod procfs;
pub mod tls_coverage;
pub mod tls_resolver;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collector_binding::{TraceBindingHandle, TraceBindingRequest};
use collector_event::{
    RawCollectorEvent, RawEventEnvelope, RawObservationPayload, RawPayloadSegment,
};
use collector_instance::{
    CollectorError, CollectorInstance, CollectorPollBatch, CollectorRawBatch,
};
use collector_stats::CollectorStats;
use config_core::daemon::{EbpfCollectorConfig, MemlockRlimit, TlsScanDebounce};
use libbpf_rs::{
    Link, MapCore, MapFlags, MapHandle, Object, ObjectBuilder, PrintLevel, RingBuffer,
    RingBufferBuilder, set_print,
};
use model_core::capability::Capability;
use model_core::ids::{CollectorName, TraceId};
use model_core::process::{ArgvCapture, HostProcessCoordinates, ProcessObservation, ProcessRecord};

use crate::capability_probe::{EbpfProbeResult, probe};
use crate::path_state::{FileObservation, PathState};
use crate::tls_resolver::{TlsDirection, TlsProbePoint};

const SESSION_LEN: usize = 128;
const CALL_ID_LEN: usize = 128;
const PROCESS_EVENT_SIZE: usize = 72 + SESSION_LEN + CALL_ID_LEN;
const EXEC_FILENAME_MAX: usize = 512;
const EXEC_ARG_MAX: usize = 16;
const EXEC_ARG_BYTES_MAX: usize = 1024;
const EXEC_EVENT_SIZE: usize =
    PROCESS_EVENT_SIZE + 8 + EXEC_FILENAME_MAX + 16 + EXEC_ARG_MAX * 8 + EXEC_ARG_BYTES_MAX;
const EVENT_FORK: u32 = 1;
const EVENT_EXEC: u32 = 2;
const EVENT_EXIT: u32 = 3;
const EVENT_NET: u32 = 4;
const EVENT_FILE: u32 = 5;
const EVENT_IPC: u32 = 6;
const EVENT_SIGNAL: u32 = 8;
const EVENT_TLS: u32 = 9;
const EVENT_FD_IO: u32 = 10;
const FD_IO_PAYLOAD_MAX: usize = 256;
const FD_IO_EVENT_SIZE: usize = PROCESS_EVENT_SIZE + 8 + FD_IO_PAYLOAD_MAX;
const FILE_PATH_MAX: usize = 512;
const FILE_EVENT_SIZE: usize = PROCESS_EVENT_SIZE + 16 + FILE_PATH_MAX * 2;
const NET_ENDPOINT_MAX: usize = 128;
const NET_EVENT_SIZE: usize = PROCESS_EVENT_SIZE + 8 + NET_ENDPOINT_MAX;
const TLS_PAYLOAD_MAX: usize = 4096;
const TLS_EVENT_SIZE: usize = PROCESS_EVENT_SIZE + 48 + TLS_PAYLOAD_MAX;
/// Bound one userspace drain so decoding and spooling cannot monopolize the
/// collector thread while the kernel ring buffer is receiving new events.
const MAX_EVENTS_PER_POLL: usize = 1024;

fn libbpf_log(level: PrintLevel, message: String) {
    let message = message.trim_end();
    match level {
        PrintLevel::Warn => tracing::warn!(target: "ebpf", "libbpf: {message}"),
        PrintLevel::Info => tracing::info!(target: "ebpf", "libbpf: {message}"),
        PrintLevel::Debug => tracing::debug!(target: "ebpf", "libbpf: {message}"),
    }
}

/// Failure while loading, attaching, or interacting with eBPF programs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoaderError {
    pub stage: String,
    pub message: String,
}

impl LoaderError {
    fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

struct EbpfRuntime {
    _object: Object,
    _links: Vec<Link>,
    /// Dynamic TLS links are keyed by trace, process generation, and image.
    /// Keeping ownership here makes dropping a trace (or a PID lifetime) remove
    /// only its probes; links are never shared across trace owners.
    tls_links: BTreeMap<(u64, u32, u64, String), Vec<Link>>,
    trace_bindings: MapHandle,
    session_ids: MapHandle,
    call_ids: MapHandle,
    loss_counters: MapHandle,
    events: Arc<Mutex<Vec<Vec<u8>>>>,
    event_buffer: RingBuffer<'static>,
    decode_failures: Arc<Mutex<u64>>,
}

impl EbpfRuntime {
    fn load(
        config: &EbpfCollectorConfig,
        requested: &BTreeSet<Capability>,
    ) -> Result<Self, LoaderError> {
        set_print(Some((PrintLevel::Warn, libbpf_log)));
        apply_memlock_rlimit(config.memlock_rlimit)?;
        let mut open = ObjectBuilder::default()
            .open_memory(include_bytes!(env!("EBPF_OBJECT")))
            .map_err(|error| LoaderError::new("open_bpf", error.to_string()))?;
        resize_map(
            &mut open,
            "trace_bindings",
            config.tracked_process_max_entries,
        )?;
        resize_map(&mut open, "session_ids", config.tracked_process_max_entries)?;
        resize_map(&mut open, "call_ids", config.tracked_process_max_entries)?;
        resize_map(
            &mut open,
            "pending_exit_ops",
            config.pending_operation_max_entries,
        )?;
        resize_map(
            &mut open,
            "pending_argv",
            config.pending_operation_max_entries,
        )?;
        resize_map(
            &mut open,
            "pending_tls_ops",
            config.pending_operation_max_entries,
        )?;
        resize_map(
            &mut open,
            "pending_net_ops",
            config.pending_operation_max_entries,
        )?;
        resize_map(
            &mut open,
            "pending_file_ops",
            config.pending_operation_max_entries,
        )?;
        resize_map(
            &mut open,
            "pending_ipc_ops",
            config.pending_operation_max_entries,
        )?;
        resize_map(
            &mut open,
            "pending_fd_io_ops",
            config.pending_operation_max_entries,
        )?;
        resize_map(&mut open, "events", config.event_ring_buffer_max_bytes)?;
        resize_map(&mut open, "loss_counters", 8)?;

        // Disable programs with no requested capability before kernel load, so
        // a process-only trace never autoloads file/network programs.  The
        // merged fd-io pair serves three capabilities, so any one requested
        // enables it; userspace fan-out re-applies the per-class gate.
        // Pipe/pipe2 and socketpair probes are internal channel infrastructure
        // (root<->worker channel identification, fd-class exclusion) and stay
        // attached at every collection level even though IPC only becomes an
        // audit capability at L3 (IPC audit output is suppressed at ingest
        // for lower levels, not here).
        let mut autoloaded = BTreeSet::new();
        for mut program in open.progs_mut() {
            let name = program.name().to_string_lossy();
            let enabled = is_infrastructure_program(name.as_ref())
                || program_capabilities(name.as_ref()).is_some_and(|capabilities| {
                    capabilities
                        .iter()
                        .any(|capability| requested.contains(capability))
                });
            if enabled {
                autoloaded.insert(name.into_owned());
            } else {
                program.set_autoload(false);
            }
        }
        let mut instruction_total = 0usize;
        let mut instruction_counts = Vec::new();
        for program in open.progs_mut() {
            let name = program.name().to_string_lossy().into_owned();
            if autoloaded.contains(&name) {
                let count = program.insn_cnt();
                instruction_total = instruction_total.saturating_add(count);
                instruction_counts.push(format!("{name}={count}"));
            }
        }
        tracing::info!(
            target: "ebpf",
            tracked_process_max_entries = config.tracked_process_max_entries,
            pending_operation_max_entries = config.pending_operation_max_entries,
            event_ring_buffer_max_bytes = config.event_ring_buffer_max_bytes,
            instruction_total,
            autoloaded_programs = ?instruction_counts,
            "loading eBPF object"
        );
        let object = open.load().map_err(|error| {
            LoaderError::new(
                "load_bpf",
                format!(
                    "{error}; maps: tracked_process_max_entries={}, pending_operation_max_entries={}, event_ring_buffer_max_bytes={}; requested_capabilities={requested:?}; autoloaded_programs={autoloaded:?}",
                    config.tracked_process_max_entries,
                    config.pending_operation_max_entries,
                    config.event_ring_buffer_max_bytes,
                ),
            )
        })?;
        let trace_bindings = map_handle(&object, "trace_bindings")?;
        let session_ids = map_handle(&object, "session_ids")?;
        let call_ids = map_handle(&object, "call_ids")?;
        let loss_counters = map_handle(&object, "loss_counters")?;
        let events_map = map_handle(&object, "events")?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let decode_failures = Arc::new(Mutex::new(0));
        let callback_events = Arc::clone(&events);
        let mut builder = RingBufferBuilder::new();
        builder
            .add(&events_map, move |raw| {
                callback_events
                    .lock()
                    .expect("ring buffer events mutex")
                    .push(raw.to_vec());
                0
            })
            .map_err(|error| LoaderError::new("ring_buffer", error.to_string()))?;
        let event_buffer = builder
            .build()
            .map_err(|error| LoaderError::new("ring_buffer", error.to_string()))?;

        // Attach only programs that were actually autoloaded: programs gated
        // off by the requested capability set have no loaded FD, and libbpf
        // would otherwise fail with "can't attach BPF program without FD".
        // TLS uprobe programs are never statically attached here (they are
        // attached dynamically per process by attach_tls).
        let mut links = Vec::new();
        for program in object.progs_mut() {
            let name = program.name().to_string_lossy().into_owned();
            if name.starts_with("handle_tls_") || !autoloaded.contains(&name) {
                continue;
            }
            links.push(
                program
                    .attach()
                    .map_err(|error| LoaderError::new("attach_bpf", format!("{name}: {error}")))?,
            );
        }
        if links.is_empty() {
            return Err(LoaderError::new(
                "attach_bpf",
                "no process lifecycle program attached",
            ));
        }
        Ok(Self {
            _object: object,
            _links: links,
            tls_links: BTreeMap::new(),
            trace_bindings,
            session_ids,
            call_ids,
            loss_counters,
            events,
            event_buffer,
            decode_failures,
        })
    }

    fn track(&self, pid: u32, start_time: u64, trace_id: TraceId) -> Result<(), LoaderError> {
        /* Matches the packed-free C layout of censorscope_trace_binding:
         * trace_id (8) + child_generation (8) + parent_pid (4) + padding (4). */
        let mut value = [0_u8; 24];
        value[..8].copy_from_slice(&trace_id.get().to_ne_bytes());
        value[8..16].copy_from_slice(&start_time.to_ne_bytes());
        // parent_pid 0 marks the binding as fully tracked/promoted.
        self.trace_bindings
            .update(&pid.to_ne_bytes(), &value, MapFlags::ANY)
            .map_err(|error| LoaderError::new("track_pid", error.to_string()))
    }

    fn cache_session(&self, pid: u32, session: &str) -> Result<(), LoaderError> {
        let mut value = [0_u8; SESSION_LEN];
        let bytes = session.as_bytes();
        let length = bytes.len().min(SESSION_LEN.saturating_sub(1));
        value[..length].copy_from_slice(&bytes[..length]);
        self.session_ids
            .update(&pid.to_ne_bytes(), &value, MapFlags::ANY)
            .map_err(|error| LoaderError::new("cache_session", error.to_string()))
    }

    fn attach_tls(
        &mut self,
        trace_id: TraceId,
        pid: u32,
        generation: u64,
        path: &Path,
        points: &[TlsProbePoint],
    ) -> Result<usize, LoaderError> {
        let key = (trace_id.get(), pid, generation, path.display().to_string());
        if self.tls_links.contains_key(&key) {
            return Ok(0);
        }
        let mut attached = 0;
        let mut links = Vec::new();
        for point in points {
            let program_names: &[(&str, bool)] = match (&point.symbol[..], point.direction) {
                ("SSL_write", TlsDirection::Outbound) => &[("handle_tls_ssl_write_enter", false)],
                ("SSL_write_ex", TlsDirection::Outbound) => &[
                    ("handle_tls_ssl_write_ex_enter", false),
                    ("handle_tls_ssl_write_ex_exit", true),
                ],
                ("SSL_read", TlsDirection::Inbound) => &[
                    ("handle_tls_ssl_read_enter", false),
                    ("handle_tls_ssl_read_exit", true),
                ],
                ("SSL_read_ex", TlsDirection::Inbound) => &[
                    ("handle_tls_ssl_read_ex_enter", false),
                    ("handle_tls_ssl_read_ex_exit", true),
                ],
                ("gnutls_record_send", TlsDirection::Outbound) => {
                    &[("handle_tls_gnutls_send_enter", false)]
                }
                ("gnutls_record_recv", TlsDirection::Inbound) => &[
                    ("handle_tls_gnutls_recv_enter", false),
                    ("handle_tls_gnutls_recv_exit", true),
                ],
                ("PR_Write", TlsDirection::Outbound) => &[("handle_tls_nss_write_enter", false)],
                ("PR_Read", TlsDirection::Inbound) => &[
                    ("handle_tls_nss_read_enter", false),
                    ("handle_tls_nss_read_exit", true),
                ],
                ("crypto/tls.(*Conn).Write", TlsDirection::Outbound) => {
                    &[("handle_tls_go_write_enter", false)]
                }
                ("crypto/tls.(*Conn).Read", TlsDirection::Inbound) => &[
                    ("handle_tls_go_read_enter", false),
                    ("handle_tls_go_read_exit", true),
                ],
                ("*rustls_write_tls", TlsDirection::Outbound) => {
                    &[("handle_tls_rustls_write_enter", false)]
                }
                ("*rustls_read_tls", TlsDirection::Inbound) => &[
                    ("handle_tls_rustls_read_enter", false),
                    ("handle_tls_rustls_read_exit", true),
                ],
                _ => continue,
            };
            for (program_name, retprobe) in program_names {
                let program = self
                    ._object
                    .progs_mut()
                    .find(|program| program.name() == OsStr::new(program_name))
                    .ok_or_else(|| {
                        LoaderError::new(
                            "attach_tls",
                            format!("BPF program {program_name} is missing"),
                        )
                    })?;
                let link = program
                    .attach_uprobe_with_opts(
                        pid as i32,
                        path,
                        usize::try_from(point.file_offset).map_err(|_| {
                            LoaderError::new("attach_tls", "TLS probe offset overflows usize")
                        })?,
                        libbpf_rs::UprobeOpts {
                            retprobe: *retprobe,
                            ..Default::default()
                        },
                    )
                    .map_err(|error| {
                        LoaderError::new(
                            "attach_tls",
                            format!("attach {program_name} to {}: {error}", path.display()),
                        )
                    })?;
                links.push(link);
                attached += 1;
            }
        }
        if attached > 0 {
            self.tls_links.insert(key, links);
        }
        Ok(attached)
    }

    fn tls_attached(&self, trace_id: TraceId, pid: u32, generation: u64, path: &Path) -> bool {
        self.tls_links
            .contains_key(&(trace_id.get(), pid, generation, path.display().to_string()))
    }

    fn detach_tls_process(&mut self, trace_id: TraceId, pid: u32, generation: u64) {
        self.tls_links
            .retain(|(trace, linked_pid, linked_generation, _), _| {
                !(*trace == trace_id.get()
                    && *linked_pid == pid
                    && *linked_generation == generation)
            });
    }

    fn detach_tls_image(&mut self, trace_id: TraceId, pid: u32, generation: u64, path: &Path) {
        let path = path.display().to_string();
        self.tls_links.retain(
            |(linked_trace, linked_pid, linked_generation, linked_path), _| {
                !(*linked_trace == trace_id.get()
                    && *linked_pid == pid
                    && *linked_generation == generation
                    && *linked_path == path)
            },
        );
    }

    fn detach_tls_trace(&mut self, trace_id: TraceId) {
        self.tls_links
            .retain(|(trace, _, _, _), _| *trace != trace_id.get());
    }

    fn untrack(&self, pid: u32) -> Result<(), LoaderError> {
        let key = pid.to_ne_bytes();
        if self
            .trace_bindings
            .lookup(&key, MapFlags::ANY)
            .map_err(|error| LoaderError::new("untrack_pid", error.to_string()))?
            .is_some()
        {
            self.trace_bindings
                .delete(&key)
                .map_err(|error| LoaderError::new("untrack_pid", error.to_string()))?;
        }
        if self
            .session_ids
            .lookup(&key, MapFlags::ANY)
            .map_err(|error| LoaderError::new("untrack_pid", error.to_string()))?
            .is_some()
        {
            self.session_ids
                .delete(&key)
                .map_err(|error| LoaderError::new("untrack_pid", error.to_string()))?;
        }
        if self
            .call_ids
            .lookup(&key, MapFlags::ANY)
            .map_err(|error| LoaderError::new("untrack_pid", error.to_string()))?
            .is_some()
        {
            self.call_ids
                .delete(&key)
                .map_err(|error| LoaderError::new("untrack_pid", error.to_string()))?;
        }
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<KernelProcessEvent>, LoaderError> {
        let consumed = self.event_buffer.consume_raw_n(MAX_EVENTS_PER_POLL);
        if consumed < 0 {
            return Err(LoaderError::new(
                "poll_bpf",
                format!("ring buffer consume failed: {consumed}"),
            ));
        }
        let mut decoded = Vec::new();
        for raw in std::mem::take(&mut *self.events.lock().expect("ring buffer events mutex")) {
            match decode_event(&raw) {
                Ok(event) => decoded.push(event),
                Err(_) => {
                    *self.decode_failures.lock().expect("decode failures mutex") += 1;
                }
            }
        }
        Ok(decoded)
    }

    fn poll_raw(&mut self) -> Result<Vec<Vec<u8>>, LoaderError> {
        let consumed = self.event_buffer.consume_raw_n(MAX_EVENTS_PER_POLL);
        if consumed < 0 {
            return Err(LoaderError::new(
                "poll_bpf",
                format!("ring buffer consume failed: {consumed}"),
            ));
        }
        Ok(std::mem::take(
            &mut *self.events.lock().expect("ring buffer events mutex"),
        ))
    }

    fn decode_failures(&self) -> u64 {
        *self.decode_failures.lock().expect("decode failures mutex")
    }

    /// Loss counts per observation class.
    ///
    /// The classes are the kernel's loss-counter keys. They are reported
    /// separately because the failures are not interchangeable: a lost process
    /// tree edge changes attribution, while lost plaintext only shortens a
    /// captured body, and a single total cannot tell the two apart.
    fn loss_counters(&self) -> Vec<(&'static str, u64)> {
        const CLASSES: [(u32, &str); 7] = [
            (1, "loss_process"),
            (2, "loss_file"),
            (3, "loss_net"),
            (4, "loss_ipc"),
            (5, "loss_fd_io"),
            (6, "loss_tls_plaintext"),
            (7, "loss_exec_context"),
        ];
        CLASSES
            .iter()
            .filter_map(|(key, name)| {
                let count = self.loss_counter(*key);
                (count > 0).then_some((*name, count))
            })
            .collect()
    }

    fn loss_counter(&self, key: u32) -> u64 {
        self.loss_counters
            .lookup(&key.to_ne_bytes(), MapFlags::ANY)
            .ok()
            .flatten()
            .and_then(|bytes| {
                let value = bytes.get(..8)?;
                let array: [u8; 8] = value.try_into().ok()?;
                Some(u64::from_ne_bytes(array))
            })
            .unwrap_or(0)
    }
}

/// Programs kept attached regardless of the requested capability set because
/// their events feed internal channel bookkeeping: pipe/socketpair creation
/// gives the userspace fd-class knowledge (socketpair exclusion from net/file
/// fan-out) and the daemon's root<->worker channel identification used by
/// orchestration-noise folding. IPC stays an *audit* capability only from L3;
/// below that its observations are consumed internally and suppressed at
/// ingest.
fn is_infrastructure_program(name: &str) -> bool {
    name.starts_with("handle_sys_enter_pipe")
        || name.starts_with("handle_sys_exit_pipe")
        || name.starts_with("handle_sys_enter_socketpair")
        || name.starts_with("handle_sys_exit_socketpair")
}

/// Capabilities served by one BPF program.  A program is autoloaded when at
/// least one of its listed capabilities is requested by the bound trace set
/// (the request union).  The merged fd-io probe pairs serve the file, network,
/// and stdio classes at once; the userspace fan-out applies the same
/// requested-set gate before emitting per-class observations.
fn program_capabilities(name: &str) -> Option<&'static [Capability]> {
    if name.ends_with("_fdio") || name.ends_with("_fdio_exit") {
        Some(&[
            Capability::FsAccessBasic,
            Capability::NetTransport,
            Capability::StdioChunk,
        ])
    } else if name.ends_with("_net") || name.ends_with("_net_exit") {
        Some(&[Capability::NetTransport])
    } else if name.ends_with("_file") || name.ends_with("_file_exit") {
        Some(&[Capability::FsAccessBasic])
    } else if name.starts_with("handle_tls_") {
        Some(&[Capability::TlsPlaintextPayload])
    } else if name.starts_with("handle_sys_enter_execve")
        || name.starts_with("handle_sys_exit_execve")
    {
        Some(&[Capability::ProcExecContext])
    } else if name.starts_with("handle_sched_process_")
        || name.starts_with("handle_sys_enter_exit")
        || name == "handle_signal_generate"
    {
        Some(&[Capability::ProcLifecycle])
    } else if name.starts_with("handle_sys_enter_connect")
        || name.starts_with("handle_sys_exit_connect")
        || name.starts_with("handle_sys_enter_accept")
        || name.starts_with("handle_sys_exit_accept")
        || name.starts_with("handle_sys_enter_sendto")
        || name.starts_with("handle_sys_exit_sendto")
        || name.starts_with("handle_sys_enter_sendmsg")
        || name.starts_with("handle_sys_exit_sendmsg")
        || name.starts_with("handle_sys_enter_recvfrom")
        || name.starts_with("handle_sys_exit_recvfrom")
        || name.starts_with("handle_sys_enter_bind")
        || name.starts_with("handle_sys_exit_bind")
        || name.starts_with("handle_sys_enter_listen")
        || name.starts_with("handle_sys_exit_listen")
    {
        Some(&[Capability::NetTransport])
    } else if name.starts_with("handle_sys_enter_mmap") || name.starts_with("handle_sys_exit_mmap")
    {
        Some(&[Capability::FsMmap])
    } else if name.starts_with("handle_sys_enter_open")
        || name.starts_with("handle_sys_exit_open")
        || name.starts_with("handle_sys_enter_creat")
        || name.starts_with("handle_sys_exit_creat")
        || name.starts_with("handle_sys_enter_unlinkat")
        || name.starts_with("handle_sys_exit_unlinkat")
        || name.starts_with("handle_sys_enter_renameat")
        || name.starts_with("handle_sys_exit_renameat")
        || name.starts_with("handle_sys_enter_mkdirat")
        || name.starts_with("handle_sys_exit_mkdirat")
        || name.starts_with("handle_sys_enter_close")
        || name.starts_with("handle_sys_exit_close")
        || name.starts_with("handle_sys_enter_dup")
        || name.starts_with("handle_sys_exit_dup")
        || name.starts_with("handle_sys_enter_fcntl")
        || name.starts_with("handle_sys_exit_fcntl")
        || name.starts_with("handle_sys_enter_chdir")
        || name.starts_with("handle_sys_exit_chdir")
        || name.starts_with("handle_sys_enter_fchdir")
        || name.starts_with("handle_sys_exit_fchdir")
        || name.starts_with("handle_sys_enter_readv")
        || name.starts_with("handle_sys_exit_readv")
        || name.starts_with("handle_sys_enter_writev")
        || name.starts_with("handle_sys_exit_writev")
        || name.starts_with("handle_sys_enter_rmdir")
        || name.starts_with("handle_sys_exit_rmdir")
        || name.starts_with("handle_sys_enter_openat2")
        || name.starts_with("handle_sys_exit_openat2")
        || name.starts_with("handle_sys_enter_truncate")
        || name.starts_with("handle_sys_exit_truncate")
        || name.starts_with("handle_sys_enter_ftruncate")
        || name.starts_with("handle_sys_exit_ftruncate")
        || name.starts_with("handle_sys_enter_renameat2")
        || name.starts_with("handle_sys_exit_renameat2")
        || name.starts_with("handle_sys_enter_rename")
        || name.starts_with("handle_sys_exit_rename")
        || name.starts_with("handle_sys_enter_unlink")
        || name.starts_with("handle_sys_exit_unlink")
        || name.starts_with("handle_sys_enter_mkdir")
        || name.starts_with("handle_sys_exit_mkdir")
    {
        Some(&[Capability::FsAccessBasic])
    } else if name.starts_with("handle_sys_enter_pipe") || name.starts_with("handle_sys_exit_pipe")
    {
        Some(&[Capability::IpcPipeFifo])
    } else if name.starts_with("handle_sys_enter_socketpair")
        || name.starts_with("handle_sys_exit_socketpair")
    {
        Some(&[Capability::IpcUnixSocket])
    } else {
        None
    }
}

fn tls_coverage_diagnostic(
    trace_id: TraceId,
    observation: &ProcessObservation,
    pid: u32,
    generation: u64,
    trigger: &str,
    image: Option<tls_coverage::TlsImage>,
    state: tls_coverage::CoverageState,
    message: String,
) -> RawCollectorEvent {
    let mut metadata = BTreeMap::new();
    metadata.insert("state".to_string(), state.as_str().to_string());
    metadata.insert("pid".to_string(), pid.to_string());
    metadata.insert("generation".to_string(), generation.to_string());
    metadata.insert("trigger".to_string(), trigger.to_string());
    metadata.insert("message".to_string(), message);
    if let Some(image) = image {
        metadata.insert("library".to_string(), image.library.as_str().to_string());
        metadata.insert("path".to_string(), image.path.display().to_string());
    }
    RawCollectorEvent {
        envelope: RawEventEnvelope {
            trace_id: Some(trace_id),
            observed_at: SystemTime::now(),
            process: observation.clone(),
            collector: CollectorName::new("ebpf"),
            session_id: None,
            call_id: None,
        },
        payload: RawObservationPayload::Application {
            protocol: "tls.coverage".to_string(),
            metadata,
        },
    }
}

#[cfg(test)]
mod capability_tests {
    use std::os::fd::AsRawFd;

    use super::*;

    #[test]
    fn process_programs_have_explicit_capability_mapping() {
        assert_eq!(
            program_capabilities("handle_sched_process_exec"),
            Some(&[Capability::ProcLifecycle][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_enter_execve"),
            Some(&[Capability::ProcExecContext][..])
        );
        assert_eq!(program_capabilities("unrelated_program"), None);
        assert_eq!(
            program_capabilities("handle_sys_enter_connect"),
            Some(&[Capability::NetTransport][..])
        );
        assert_eq!(
            program_capabilities("handle_signal_generate"),
            Some(&[Capability::ProcLifecycle][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_enter_sendmsg"),
            Some(&[Capability::NetTransport][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_exit_openat"),
            Some(&[Capability::FsAccessBasic][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_enter_truncate"),
            Some(&[Capability::FsAccessBasic][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_exit_mmap"),
            Some(&[Capability::FsMmap][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_enter_pipe2"),
            Some(&[Capability::IpcPipeFifo][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_exit_socketpair"),
            Some(&[Capability::IpcUnixSocket][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_read_file"),
            Some(&[Capability::FsAccessBasic][..])
        );
        assert_eq!(
            program_capabilities("handle_sys_write_net_exit"),
            Some(&[Capability::NetTransport][..])
        );
        /* The merged fd-io probes serve file, net, and stdio at once. */
        assert_eq!(
            program_capabilities("handle_sys_read_fdio"),
            Some(
                &[
                    Capability::FsAccessBasic,
                    Capability::NetTransport,
                    Capability::StdioChunk,
                ][..]
            )
        );
        assert_eq!(
            program_capabilities("handle_sys_writev_fdio_exit"),
            Some(
                &[
                    Capability::FsAccessBasic,
                    Capability::NetTransport,
                    Capability::StdioChunk,
                ][..]
            )
        );
    }

    #[test]
    fn every_compiled_bpf_program_has_a_capability_mapping() {
        let source = include_str!("../bpf/live_observation.bpf.c");
        let programs = source.lines().filter_map(|line| {
            line.trim_start()
                .strip_prefix("int handle_")
                .and_then(|suffix| suffix.split_once('('))
                .map(|(suffix, _)| format!("handle_{suffix}"))
        });
        for program in programs {
            assert!(
                program_capabilities(&program).is_some(),
                "BPF program {program} has no capability mapping"
            );
        }
    }

    #[test]
    fn merged_fd_io_abi_decodes_operation_and_payload() {
        let mut raw = vec![0_u8; FD_IO_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_FD_IO.to_ne_bytes());
        // ABI offsets: 8 = operation (write), 12 = host_pid, 20 = result
        // (bytes written), 24 = trace_id, 40 = fd (stdout), 48 = requested_size.
        raw[8..12].copy_from_slice(&2_u32.to_ne_bytes());
        raw[12..16].copy_from_slice(&77_u32.to_ne_bytes());
        raw[20..24].copy_from_slice(&19_i32.to_ne_bytes());
        raw[24..32].copy_from_slice(&9_u64.to_ne_bytes());
        raw[40..44].copy_from_slice(&1_u32.to_ne_bytes());
        raw[48..56].copy_from_slice(&19_u64.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE..PROCESS_EVENT_SIZE + 4].copy_from_slice(&6_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 8..PROCESS_EVENT_SIZE + 14].copy_from_slice(b"stdout");
        let event = decode_event(&raw).expect("fd-io ABI decodes");
        assert_eq!(event.kind, EVENT_FD_IO);
        assert_eq!(event.fd_io_operation, Some(2));
        assert_eq!(event.fd, 1);
        assert_eq!(event.result, 19);
        assert_eq!(event.fd_io_payload.as_deref(), Some(&b"stdout"[..]));
        assert!(!event.fd_io_loss);
    }

    #[test]
    fn network_event_abi_decodes_transport_fields() {
        let mut raw = vec![0_u8; PROCESS_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_NET.to_ne_bytes());
        raw[8..12].copy_from_slice(&3_u32.to_ne_bytes());
        raw[12..16].copy_from_slice(&42_u32.to_ne_bytes());
        raw[20..24].copy_from_slice(&17_i32.to_ne_bytes());
        raw[24..32].copy_from_slice(&9_u64.to_ne_bytes());
        raw[40..44].copy_from_slice(&5_u32.to_ne_bytes());
        raw[48..56].copy_from_slice(&128_u64.to_ne_bytes());
        let event = decode_event(&raw).expect("network ABI decodes");
        assert_eq!(event.kind, EVENT_NET);
        assert_eq!(event.reserved, 3);
        assert_eq!(event.fd, 5);
        assert_eq!(event.requested_size, 128);
        assert_eq!(event.result, 17);
    }

    #[test]
    fn sendmsg_and_signal_abi_fields_remain_lossless() {
        assert_eq!(net_operation(7), "sendmsg");
        assert_eq!(
            net_direction(7),
            model_core::payload::PayloadDirection::Outbound
        );
        assert_eq!(net_length(7, 0, 19), Some(19));
        assert_eq!(net_operation(8), "write");
        assert_eq!(net_operation(11), "readv");
        assert_eq!(net_length(10, 0, 23), Some(23));

        let mut raw = vec![0_u8; PROCESS_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_SIGNAL.to_ne_bytes());
        raw[12..16].copy_from_slice(&42_u32.to_ne_bytes());
        raw[20..24].copy_from_slice(&1_i32.to_ne_bytes());
        raw[40..44].copy_from_slice(&15_u32.to_ne_bytes());
        raw[44..48].copy_from_slice(&1_u32.to_ne_bytes());
        raw[48..56].copy_from_slice(&91_u64.to_ne_bytes());
        let event = decode_event(&raw).expect("signal ABI decodes");
        assert_eq!(event.kind, EVENT_SIGNAL);
        assert_eq!(event.host_pid, 42);
        assert_eq!(event.fd, 15);
        assert_eq!(event.aux_fd, 1);
        assert_eq!(event.requested_size, 91);
        assert_eq!(event.result, 1);
    }

    #[test]
    fn file_event_abi_decodes_fd_and_requested_size() {
        let mut raw = vec![0_u8; PROCESS_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_FILE.to_ne_bytes());
        raw[8..12].copy_from_slice(&2_u32.to_ne_bytes());
        raw[12..16].copy_from_slice(&51_u32.to_ne_bytes());
        raw[20..24].copy_from_slice(&7_i32.to_ne_bytes());
        raw[24..32].copy_from_slice(&11_u64.to_ne_bytes());
        raw[40..44].copy_from_slice(&4_u32.to_ne_bytes());
        raw[48..56].copy_from_slice(&4096_u64.to_ne_bytes());
        let event = decode_event(&raw).expect("file ABI decodes");
        assert_eq!(event.kind, EVENT_FILE);
        assert_eq!(event.reserved, 2);
        assert_eq!(event.fd, 4);
        assert_eq!(event.requested_size, 4096);
        assert_eq!(event.result, 7);
    }

    #[test]
    fn exec_event_abi_preserves_argv_boundaries_and_non_utf8_bytes() {
        let mut raw = vec![0_u8; EXEC_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_EXEC.to_ne_bytes());
        let filename = b"/bin/demo";
        raw[PROCESS_EVENT_SIZE..PROCESS_EVENT_SIZE + 4]
            .copy_from_slice(&(filename.len() as u32).to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 8..PROCESS_EVENT_SIZE + 8 + filename.len()]
            .copy_from_slice(filename);
        let base = PROCESS_EVENT_SIZE + 8 + EXEC_FILENAME_MAX;
        raw[base..base + 4].copy_from_slice(&2_u32.to_ne_bytes());
        raw[base + 8..base + 12].copy_from_slice(&7_u32.to_ne_bytes());
        let entries = base + 16;
        raw[entries..entries + 4].copy_from_slice(&0_u32.to_ne_bytes());
        raw[entries + 4..entries + 8].copy_from_slice(&4_u32.to_ne_bytes());
        raw[entries + 8..entries + 12].copy_from_slice(&4_u32.to_ne_bytes());
        raw[entries + 12..entries + 16].copy_from_slice(&3_u32.to_ne_bytes());
        let bytes = entries + EXEC_ARG_MAX * 8;
        raw[bytes..bytes + 5].copy_from_slice(b"demo\0");
        raw[bytes + 4..bytes + 7].copy_from_slice(&[0xff, 0x00, 0x01]);
        let event = decode_event(&raw).expect("exec ABI decodes");
        assert_eq!(event.executable.as_deref(), Some("/bin/demo"));
        assert_eq!(
            event.argv.expect("argv present").args,
            vec![b"demo".to_vec(), vec![0xff, 0x00, 0x01]]
        );
    }

    #[test]
    fn exec_event_abi_rejects_out_of_bounds_argv_entry() {
        let mut raw = vec![0_u8; EXEC_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_EXEC.to_ne_bytes());
        let base = PROCESS_EVENT_SIZE + 8 + EXEC_FILENAME_MAX;
        raw[base..base + 4].copy_from_slice(&1_u32.to_ne_bytes());
        raw[base + 8..base + 12].copy_from_slice(&1_u32.to_ne_bytes());
        let entries = base + 16;
        raw[entries..entries + 4].copy_from_slice(&1024_u32.to_ne_bytes());
        raw[entries + 4..entries + 8].copy_from_slice(&1_u32.to_ne_bytes());
        assert!(decode_event(&raw).is_err());
    }

    #[test]
    fn tls_event_abi_decodes_direction_symbol_and_plaintext() {
        let mut raw = vec![0_u8; TLS_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_TLS.to_ne_bytes());
        raw[48..56].copy_from_slice(&4_u64.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE..PROCESS_EVENT_SIZE + 4].copy_from_slice(&4_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 8..PROCESS_EVENT_SIZE + 12].copy_from_slice(&2_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 12..PROCESS_EVENT_SIZE + 16].copy_from_slice(&2_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 16..PROCESS_EVENT_SIZE + 24]
            .copy_from_slice(&0x1234_u64.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 24..PROCESS_EVENT_SIZE + 32].copy_from_slice(&7_u64.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 40..PROCESS_EVENT_SIZE + 44].copy_from_slice(&3_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 44..PROCESS_EVENT_SIZE + 48].copy_from_slice(&8_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 48..PROCESS_EVENT_SIZE + 52].copy_from_slice(b"ping");
        let event = decode_event(&raw).expect("TLS ABI decodes");
        assert_eq!(event.kind, EVENT_TLS);
        assert_eq!(event.tls_direction, Some(2));
        assert_eq!(event.tls_symbol, Some(2));
        assert_eq!(event.tls_payload.as_deref(), Some(b"ping".as_slice()));
        assert_eq!(event.tls_connection, 0x1234);
        assert_eq!(event.tls_call_id, 7);
        assert_eq!(event.tls_chunk_index, 3);
        assert_eq!(event.tls_chunk_flags, 8);
        assert_eq!(event.requested_size, 4);
    }

    #[test]
    fn ipc_event_abi_decodes_fd_pair() {
        let mut raw = vec![0_u8; PROCESS_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_IPC.to_ne_bytes());
        raw[8..12].copy_from_slice(&1_u32.to_ne_bytes());
        raw[12..16].copy_from_slice(&88_u32.to_ne_bytes());
        raw[40..44].copy_from_slice(&3_u32.to_ne_bytes());
        raw[44..48].copy_from_slice(&4_u32.to_ne_bytes());
        let event = decode_event(&raw).expect("ipc ABI decodes");
        assert_eq!(event.kind, EVENT_IPC);
        assert_eq!(event.reserved, 1);
        assert_eq!(event.fd, 3);
        assert_eq!(event.aux_fd, 4);
    }

    #[test]
    fn file_path_event_abi_decodes_both_paths_and_truncation() {
        let mut raw = vec![0_u8; FILE_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_FILE.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE..PROCESS_EVENT_SIZE + 4].copy_from_slice(&5_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 4..PROCESS_EVENT_SIZE + 8].copy_from_slice(&1_u32.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 8..PROCESS_EVENT_SIZE + 12].copy_from_slice(&3_u32.to_ne_bytes());
        let base = PROCESS_EVENT_SIZE + 16;
        raw[base..base + 5].copy_from_slice(b"/tmp/");
        raw[base + FILE_PATH_MAX..base + FILE_PATH_MAX + 3].copy_from_slice(b"new");
        let event = decode_event(&raw).expect("file path ABI decodes");
        assert_eq!(event.file_path.as_deref(), Some("/tmp/"));
        assert_eq!(event.file_path2.as_deref(), Some("new"));
        assert!(event.file_path_truncated);
    }

    #[test]
    fn file_path_event_abi_preserves_capture_gap() {
        let mut raw = vec![0_u8; FILE_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_FILE.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE + 4..PROCESS_EVENT_SIZE + 8].copy_from_slice(&2_u32.to_ne_bytes());
        let event = decode_event(&raw).expect("file path gap ABI decodes");
        assert!(event.file_path_capture_gap);
        assert!(!event.file_path_truncated);
    }

    #[test]
    fn network_endpoint_abi_decodes_ipv4() {
        let mut raw = vec![0_u8; NET_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_NET.to_ne_bytes());
        raw[PROCESS_EVENT_SIZE..PROCESS_EVENT_SIZE + 4].copy_from_slice(&8_u32.to_ne_bytes());
        let base = PROCESS_EVENT_SIZE + 8;
        raw[base..base + 2].copy_from_slice(&2_u16.to_ne_bytes());
        raw[base + 2..base + 4].copy_from_slice(&443_u16.to_be_bytes());
        raw[base + 4..base + 8].copy_from_slice(&[127, 0, 0, 1]);
        let event = decode_event(&raw).expect("network endpoint ABI decodes");
        assert_eq!(
            decode_endpoint(event.net_endpoint.as_deref().unwrap()),
            Some(("ipv4".to_string(), "127.0.0.1:443".to_string()))
        );
    }

    #[test]
    fn network_endpoint_decoder_handles_ipv6_and_rejects_unknown_family() {
        let mut bytes = vec![0_u8; 24];
        bytes[0..2].copy_from_slice(&10_u16.to_ne_bytes());
        bytes[2..4].copy_from_slice(&8443_u16.to_be_bytes());
        bytes[8..24].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(
            decode_endpoint(&bytes),
            Some(("ipv6".to_string(), "[::1]:8443".to_string()))
        );
        bytes[0..2].copy_from_slice(&999_u16.to_ne_bytes());
        assert_eq!(decode_endpoint(&bytes), None);
    }

    #[test]
    fn event_abi_decodes_in_kernel_session_id() {
        let mut raw = vec![0_u8; PROCESS_EVENT_SIZE];
        raw[0..4].copy_from_slice(&EVENT_NET.to_ne_bytes());
        raw[72..72 + 6].copy_from_slice(b"sess-a");
        raw[72 + SESSION_LEN..72 + SESSION_LEN + 6].copy_from_slice(b"call-a");
        let event = decode_event(&raw).expect("session ABI decodes");
        assert_eq!(
            event.session_id,
            Some(model_core::process::SessionIdentity::new("sess-a"))
        );
        assert_eq!(event.call_id.as_deref(), Some("call-a"));
    }

    #[test]
    fn proc_fd_fallback_distinguishes_sockets_from_regular_files() {
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let file = std::fs::File::open("/dev/null").expect("open /dev/null");
        let pid = std::process::id();
        assert!(fd_is_socket(pid, socket.as_raw_fd() as u32));
        assert!(!fd_is_socket(pid, file.as_raw_fd() as u32));
        assert_eq!(fd_probe_kind(pid, socket.as_raw_fd() as u32), Some(true));
        assert_eq!(fd_probe_kind(pid, file.as_raw_fd() as u32), Some(false));
        // A dead fd is undeterminable rather than being misread as a socket.
        assert_eq!(fd_probe_kind(pid, u32::MAX - 1), None);
    }

    #[test]
    fn fd_class_prefers_capture_order_file_knowledge_over_live_probe() {
        // Capture-order file knowledge wins over a live probe: an fd recycled
        // to a socket before decode must not flip the fd-io class to net.
        assert!(!fd_is_socket_class(true, true, Some(true)));
        assert!(!fd_is_socket_class(false, true, Some(true)));
        assert!(!fd_is_socket_class(false, true, Some(false)));
        assert!(!fd_is_socket_class(false, true, None));
        // Learned socket stays socket even if the probe disagrees or fails.
        assert!(fd_is_socket_class(true, false, Some(false)));
        assert!(fd_is_socket_class(true, false, None));
        // Unknown fds fall back to the probe result, defaulting to file.
        assert!(fd_is_socket_class(false, false, Some(true)));
        assert!(!fd_is_socket_class(false, false, Some(false)));
        assert!(!fd_is_socket_class(false, false, None));
    }
}

#[derive(Clone, Debug)]
struct KernelProcessEvent {
    kind: u32,
    host_pid: u32,
    child_host_pid: u32,
    result: i32,
    trace_id: TraceId,
    /// Kernel monotonic timestamp captured when the syscall event occurred.
    observed_ktime_ns: u64,
    reserved: u32,
    generation: u64,
    child_generation: u64,
    executable: Option<String>,
    argv: Option<ArgvCapture>,
    fd: u32,
    aux_fd: u32,
    requested_size: u64,
    file_path: Option<String>,
    file_path2: Option<String>,
    file_path_truncated: bool,
    file_path_capture_gap: bool,
    net_endpoint: Option<Vec<u8>>,
    net_endpoint_loss: bool,
    tls_direction: Option<u32>,
    tls_symbol: Option<u32>,
    tls_payload: Option<Vec<u8>>,
    tls_flags: u32,
    tls_connection: u64,
    tls_call_id: u64,
    tls_chunk_offset: u64,
    tls_chunk_index: u32,
    tls_chunk_flags: u32,
    fd_io_operation: Option<u32>,
    fd_io_payload: Option<Vec<u8>>,
    fd_io_loss: bool,
    session_id: Option<model_core::process::SessionIdentity>,
    call_id: Option<String>,
}

/// Convert the kernel monotonic timestamp in an event to Unix time.
///
/// The conversion samples both clocks at decode time.  This preserves event
/// ordering and avoids attributing a delayed ring-buffer decode to the wrong
/// tool-call window.
fn event_observed_at(event: &KernelProcessEvent) -> SystemTime {
    if event.observed_ktime_ns == 0 {
        return UNIX_EPOCH;
    }
    let mut monotonic = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut realtime = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC and CLOCK_REALTIME are available on every supported
    // Linux target.  A failed sample is safer as epoch than as decode-time.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut monotonic) } != 0
        || unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut realtime) } != 0
    {
        return UNIX_EPOCH;
    }
    let mono_ns = (monotonic.tv_sec as i128)
        .saturating_mul(1_000_000_000)
        .saturating_add(monotonic.tv_nsec as i128);
    let real_ns = (realtime.tv_sec as i128)
        .saturating_mul(1_000_000_000)
        .saturating_add(realtime.tv_nsec as i128);
    let epoch_ns = real_ns.saturating_add(event.observed_ktime_ns as i128 - mono_ns);
    if epoch_ns <= 0 {
        return UNIX_EPOCH;
    }
    UNIX_EPOCH + std::time::Duration::from_nanos(epoch_ns as u64)
}

/// Collector that binds trace PIDs to eBPF maps and emits fork/exec/exit events.
pub struct EbpfCollector {
    config: EbpfCollectorConfig,
    probe_result: EbpfProbeResult,
    runtime: Option<EbpfRuntime>,
    processes: BTreeMap<u32, (TraceId, ProcessObservation)>,
    trace_pids: BTreeMap<TraceId, BTreeSet<u32>>,
    requested_capabilities: BTreeSet<Capability>,
    payload_segments: Vec<RawPayloadSegment>,
    socket_fds: BTreeSet<(u32, u64, u32)>,
    ipc_unix_fds: BTreeSet<(u32, u64, u32)>,
    /// Regular-file fds learned in capture order from open/openat/creat/
    /// openat2 and dup success.  Kept separate from `socket_fds` so fd-io
    /// fan-out can classify a tracked file without probing live /proc, whose
    /// answer is wrong whenever the fd number was recycled before decode.
    file_fds: BTreeSet<(u32, u64, u32)>,
    path_state: PathState,
    pending_tls_diagnostics: Vec<RawCollectorEvent>,
    tls_sequences: BTreeMap<(u32, u64, u64, u32), u64>,
    /// Userspace stdio payload sequencing for merged fd-io events, keyed by
    /// (host pid, fd) — mirrors the kernel-side counter the stdio probes used.
    stdio_sequences: BTreeMap<(u32, u32), u64>,
    tls_last_refresh: BTreeMap<(TraceId, u32, u64), SystemTime>,
    tls_image_scans: BTreeMap<(TraceId, u32, u64, String), SystemTime>,
    offline_decoder: bool,
}

impl EbpfCollector {
    /// Create a lazy collector; kernel programs load on the first trace binding.
    pub fn new(config: EbpfCollectorConfig) -> Self {
        let mut probe_result = probe();
        if !config.enabled {
            probe_result.reason_unavailable =
                Some("collector disabled by configuration".to_string());
        }
        Self {
            config,
            probe_result,
            runtime: None,
            processes: BTreeMap::new(),
            trace_pids: BTreeMap::new(),
            requested_capabilities: [Capability::ProcLifecycle].into_iter().collect(),
            payload_segments: Vec::new(),
            socket_fds: BTreeSet::new(),
            ipc_unix_fds: BTreeSet::new(),
            file_fds: BTreeSet::new(),
            path_state: PathState::new(),
            pending_tls_diagnostics: Vec::new(),
            tls_sequences: BTreeMap::new(),
            stdio_sequences: BTreeMap::new(),
            tls_last_refresh: BTreeMap::new(),
            tls_image_scans: BTreeMap::new(),
            offline_decoder: false,
        }
    }

    /// Construct a decoder that never loads or modifies eBPF state.
    pub fn new_decoder(
        config: EbpfCollectorConfig,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self {
        let mut collector = Self::new(config);
        collector.offline_decoder = true;
        collector.requested_capabilities.extend(capabilities);
        collector
    }

    pub fn seed_decode_memberships(
        &mut self,
        trace_id: TraceId,
        records: impl IntoIterator<Item = ProcessRecord>,
    ) {
        for record in records {
            let _ = self.track_observation(trace_id, record.observation());
        }
    }

    pub fn decode_raw_events(
        &mut self,
        raws: Vec<Vec<u8>>,
    ) -> Result<CollectorPollBatch, CollectorError> {
        let mut events = Vec::with_capacity(raws.len());
        for raw in raws {
            events.push(decode_event(&raw).map_err(collector_error)?);
        }
        let observations = self.decode_batch(events);
        Ok(CollectorPollBatch {
            observations,
            payload_segments: std::mem::take(&mut self.payload_segments),
        })
    }

    pub fn unbind_decode_trace(&mut self, trace_id: TraceId) {
        let _ = self.unbind_trace(trace_id);
    }

    pub fn probe_result(&self) -> &EbpfProbeResult {
        &self.probe_result
    }

    /// Returns the build-selected kernel-to-userspace event transport.
    pub const fn event_transport(&self) -> &'static str {
        env!("EBPF_EVENT_TRANSPORT")
    }

    /// Seed all processes found by the initial procfs snapshot into eBPF maps.
    pub fn seed_trace_memberships(
        &mut self,
        trace_id: TraceId,
        records: impl IntoIterator<Item = ProcessRecord>,
    ) -> Result<(), CollectorError> {
        for record in records {
            let Some(host) = record.host.clone() else {
                continue;
            };
            let generation = host.start_boottime_ns.unwrap_or(host.start_time_ticks);
            // Root is refreshed by bind_trace(); descendants already in the
            // initial snapshot emit no new event after seeding, so refresh
            // newly seeded processes explicitly below.
            let already_tracked = self.processes.get(&host.pid).is_some_and(|(trace, value)| {
                *trace == trace_id
                    && value
                        .host
                        .as_ref()
                        .map(|item| {
                            item.start_boottime_ns.unwrap_or(item.start_time_ticks) == generation
                        })
                        .unwrap_or(false)
            });
            let observation = ProcessObservation {
                host: Some(host.clone()),
                namespace: record.namespaces.iter().next().cloned(),
            };
            self.track_observation(trace_id, observation)?;
            if !already_tracked {
                let observation = self
                    .processes
                    .get(&host.pid)
                    .map(|(_, value)| value.clone())
                    .expect("seeded process was tracked");
                self.refresh_tls_for_process(trace_id, &observation, "seed");
            }
        }
        Ok(())
    }

    fn ensure_runtime(&mut self) -> Result<&mut EbpfRuntime, CollectorError> {
        if let Some(reason) = &self.probe_result.reason_unavailable {
            return Err(CollectorError::new("ebpf_unavailable", reason.clone()));
        }
        if self.runtime.is_none() {
            self.runtime = Some(
                EbpfRuntime::load(&self.config, &self.requested_capabilities)
                    .map_err(collector_error)?,
            );
        }
        Ok(self.runtime.as_mut().expect("runtime initialized"))
    }

    fn track_observation(
        &mut self,
        trace_id: TraceId,
        observation: ProcessObservation,
    ) -> Result<(), CollectorError> {
        let host = observation.host.as_ref().ok_or_else(|| {
            CollectorError::new("track_pid", "host process coordinates are missing")
        })?;
        let pid = host.pid;
        let generation = host.start_boottime_ns.unwrap_or(host.start_time_ticks);
        if !self.offline_decoder {
            self.ensure_runtime()?
                .track(pid, generation, trace_id)
                .map_err(collector_error)?;
        }
        /* Seed already-running processes as well as fork-created ones.  New
         * tool-call children are populated in the fork hook; this bootstrap
         * covers a trace attached after the child was created. */
        if !self.offline_decoder {
            if let Some(session) =
                crate::procfs::read_process_session(pid, "CENSORSCOPE_SESSION_ID")
                    .or_else(|| crate::procfs::read_process_session(pid, "DSH_SESSION_ID"))
            {
                self.ensure_runtime()?
                    .cache_session(pid, session.as_str())
                    .map_err(collector_error)?;
            }
        }
        self.processes.insert(pid, (trace_id, observation));
        self.trace_pids.entry(trace_id).or_default().insert(pid);
        Ok(())
    }

    fn observation(&self, pid: u32, generation: u64) -> ProcessObservation {
        self.processes
            .get(&pid)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| {
                ProcessObservation::host(
                    HostProcessCoordinates::new(pid, 0).with_start_boottime_ns(generation),
                )
            })
    }

    /// Discover and attach TLS probes for one process lifetime. Discovery is
    /// best-effort and always emits a coverage diagnostic; a process is never
    /// reported as covered until libbpf returns successful links.
    fn refresh_tls_for_process(
        &mut self,
        trace_id: TraceId,
        observation: &ProcessObservation,
        trigger: &str,
    ) {
        if self.offline_decoder {
            return;
        }
        if !self
            .requested_capabilities
            .contains(&Capability::TlsPlaintextPayload)
        {
            return;
        }
        let Some(host) = observation.host.as_ref() else {
            return;
        };
        let generation = host.start_boottime_ns.unwrap_or(host.start_time_ticks);
        let process_key = (trace_id, host.pid, generation);
        let now = SystemTime::now();
        if trigger == "mmap"
            && matches!(self.config.tls_scan_debounce, TlsScanDebounce::Interval)
            && self.tls_last_refresh.get(&process_key).is_some_and(|last| {
                now.duration_since(*last).unwrap_or_default()
                    < Duration::from_millis(self.config.tls_scan_interval_ms)
            })
        {
            return;
        }
        if trigger == "mmap" {
            self.tls_last_refresh.insert(process_key, now);
        }
        let images = match tls_coverage::discover_process_images(host.pid) {
            Ok(images) => images,
            Err(message) => {
                self.pending_tls_diagnostics.push(tls_coverage_diagnostic(
                    trace_id,
                    observation,
                    host.pid,
                    generation,
                    trigger,
                    None,
                    tls_coverage::CoverageState::ResolveFailure,
                    message,
                ));
                return;
            }
        };
        // mmap/exec notifications can observe an image unload as well as a
        // load. Remove links whose backing image is no longer present so a
        // later PID reuse or a reloaded library cannot inherit stale probes.
        let mut stale_images = Vec::new();
        if let Some(runtime) = self.runtime.as_mut() {
            let current = images
                .iter()
                .map(|image| image.path.display().to_string())
                .collect::<BTreeSet<_>>();
            let stale = runtime
                .tls_links
                .keys()
                .filter(|(trace, pid, linked_generation, path)| {
                    *trace == trace_id.get()
                        && *pid == host.pid
                        && *linked_generation == generation
                        && !current.contains(path)
                })
                .map(|(_, _, _, path)| Path::new(path).to_path_buf())
                .collect::<Vec<_>>();
            for path in stale {
                runtime.detach_tls_image(trace_id, host.pid, generation, &path);
                stale_images.push(path);
            }
        }
        for path in stale_images {
            self.tls_image_scans.remove(&(
                trace_id,
                host.pid,
                generation,
                path.display().to_string(),
            ));
        }
        for record in tls_coverage::discovery_records_from_images(host.pid, generation, images) {
            if matches!(self.config.tls_scan_debounce, TlsScanDebounce::Image) {
                if let Some(image) = &record.image {
                    self.tls_image_scans.insert(
                        (
                            trace_id,
                            host.pid,
                            generation,
                            image.path.display().to_string(),
                        ),
                        now,
                    );
                }
            }
            let (mut record, probe_points) = record.resolve_probe_plan();
            let mut already_attached = false;
            if let (Some(image), Some(points)) = (&record.image, probe_points.as_ref()) {
                let attached_before = self.runtime.as_ref().is_some_and(|runtime| {
                    runtime.tls_attached(trace_id, host.pid, generation, &image.path)
                });
                if attached_before {
                    already_attached = true;
                    record = record.attached();
                    record.message = "TLS uprobe already attached for this image".to_string();
                } else {
                    let attach_result = match self.ensure_runtime() {
                        Ok(runtime) => {
                            runtime.attach_tls(trace_id, host.pid, generation, &image.path, points)
                        }
                        Err(error) => Err(LoaderError::new(error.stage, error.message)),
                    };
                    match attach_result {
                        Ok(attached) if attached > 0 => record = record.attached(),
                        Ok(_) => {
                            record = record.attach_failed("TLS probe plan had no attachable points")
                        }
                        Err(error) => record = record.attach_failed(error.message),
                    }
                }
            }
            let mut metadata = BTreeMap::new();
            metadata.insert("state".to_string(), record.state.as_str().to_string());
            metadata.insert("pid".to_string(), record.pid.to_string());
            metadata.insert("generation".to_string(), record.generation.to_string());
            metadata.insert("trigger".to_string(), trigger.to_string());
            metadata.insert("message".to_string(), record.message);
            metadata.insert(
                "coverage_start".to_string(),
                if already_attached {
                    "already_attached"
                } else if matches!(record.state, tls_coverage::CoverageState::Covered) {
                    "attachment_success"
                } else {
                    "not_covered"
                }
                .to_string(),
            );
            if let Some(image) = record.image {
                metadata.insert("library".to_string(), image.library.as_str().to_string());
                metadata.insert("path".to_string(), image.path.display().to_string());
            }
            if let Some(points) = probe_points {
                metadata.insert("probe_points".to_string(), points.len().to_string());
            }
            self.pending_tls_diagnostics.push(RawCollectorEvent {
                envelope: RawEventEnvelope {
                    trace_id: Some(trace_id),
                    observed_at: SystemTime::now(),
                    process: observation.clone(),
                    collector: CollectorName::new("ebpf"),
                    session_id: None,
                    call_id: None,
                },
                payload: RawObservationPayload::Application {
                    protocol: "tls.coverage".to_string(),
                    metadata,
                },
            });
        }
    }

    /// Fan a merged fd-io event (read/write/readv/writev on a tracked pid) into
    /// the stdio / file / network observations its fd class implies, using the
    /// fd state already maintained for the process.  Delivery of each class is
    /// gated by the same requested-capability set the autoloader uses.
    fn fan_out_fd_io(&mut self, event: KernelProcessEvent) -> Vec<RawCollectorEvent> {
        let mut output = Vec::new();
        let observed_at = event_observed_at(&event);
        let operation = match event.fd_io_operation {
            Some(1) => "read",
            Some(2) => "write",
            Some(3) => "readv",
            Some(4) => "writev",
            _ => "unknown",
        };
        let inbound = matches!(event.fd_io_operation, Some(1 | 3));
        let direction = if inbound {
            model_core::payload::PayloadDirection::Inbound
        } else {
            model_core::payload::PayloadDirection::Outbound
        };
        let fd = event.fd;
        let process = self.observation(event.host_pid, event.generation);
        let envelope = |observed_at: SystemTime, process: &ProcessObservation| RawEventEnvelope {
            trace_id: Some(event.trace_id),
            observed_at,
            process: process.clone(),
            collector: CollectorName::new("ebpf"),
            session_id: Some(event.session_id.clone()).flatten(),
            call_id: event.call_id.clone(),
        };

        if fd <= 2
            && event.result > 0
            && self
                .requested_capabilities
                .contains(&Capability::StdioChunk)
        {
            let stream = match fd {
                0 => "stdin",
                1 => "stdout",
                2 => "stderr",
                _ => "stdio",
            };
            let bytes = event.fd_io_payload.clone();
            let captured_size = bytes.as_ref().map_or(0, |value| value.len() as u64);
            let sequence = {
                let counter = self
                    .stdio_sequences
                    .entry((event.host_pid, fd))
                    .or_insert(0);
                *counter = counter.saturating_add(1);
                *counter
            };
            let mut stdio_metadata = BTreeMap::new();
            stdio_metadata.insert("sequence".to_string(), sequence.to_string());
            if event.fd_io_loss {
                stdio_metadata.insert("capture_gap".to_string(), "true".to_string());
            }
            self.payload_segments.push(RawPayloadSegment {
                envelope: envelope(observed_at, &process),
                source: model_core::payload::PayloadSourceBoundary::Stdio,
                content_state: if event.fd_io_loss {
                    model_core::payload::PayloadContentState::Loss
                } else if captured_size < event.result as u64 {
                    model_core::payload::PayloadContentState::Truncated
                } else {
                    model_core::payload::PayloadContentState::Complete
                },
                direction,
                stream_key: Some(format!("{}:{}", event.host_pid, fd)),
                sequence,
                operation_id: None,
                offset: None,
                completed: true,
                original_size: event.result as u64,
                captured_size,
                library: None,
                symbol: None,
                protocol_hint: None,
                loss_reason: event
                    .fd_io_loss
                    .then(|| "stdio user buffer read failed".to_string()),
                bytes,
            });
            output.push(RawCollectorEvent {
                envelope: envelope(observed_at, &process),
                payload: RawObservationPayload::Stdio {
                    stream: stream.to_string(),
                    direction,
                    length: Some(event.result as u64),
                    metadata: stdio_metadata,
                },
            });
        }

        let fd_key = (event.host_pid, event.generation, fd);
        if self.ipc_unix_fds.contains(&fd_key) {
            // Socketpair descriptors intentionally produce neither transport
            // nor file observations.
            return output;
        }
        // fd-class resolution: capture-order set knowledge wins over a live
        // /proc probe, whose decode-time answer lags capture and may see the
        // fd recycled to a socket, fabricating a network row for a file write.
        let known_file = self.file_fds.contains(&fd_key);
        let probed_socket = fd_probe_kind(event.host_pid, fd);
        let is_socket =
            fd_is_socket_class(self.socket_fds.contains(&fd_key), known_file, probed_socket);
        if is_socket {
            if self
                .requested_capabilities
                .contains(&Capability::NetTransport)
            {
                self.socket_fds.insert(fd_key);
                // Map fd-io opcodes onto the net-event space (8..=11:
                // write/read/writev/readv) for length semantics.
                let net_opcode = match event.fd_io_operation {
                    Some(1) => 9,
                    Some(2) => 8,
                    Some(3) => 11,
                    Some(4) => 10,
                    _ => 0,
                };
                let mut metadata = BTreeMap::new();
                metadata.insert("fd".to_string(), fd.to_string());
                output.push(RawCollectorEvent {
                    envelope: envelope(observed_at, &process),
                    payload: RawObservationPayload::Net {
                        operation: operation.to_string(),
                        endpoint: None,
                        direction,
                        length: net_length(net_opcode, event.requested_size, event.result),
                        result: Some(i64::from(event.result)),
                        metadata,
                    },
                });
            }
            return output;
        }
        if self
            .requested_capabilities
            .contains(&Capability::FsAccessBasic)
        {
            if !known_file && probed_socket == Some(false) {
                // Determinable non-socket seen for the first time: learn it so
                // later fd-io never re-probes a recycled number, and drop any
                // stale opposite classification.
                self.file_fds.insert(fd_key);
                self.socket_fds.remove(&fd_key);
                self.ipc_unix_fds.remove(&fd_key);
            }
            let resolution = self.path_state.observe(FileObservation {
                trace_id: event.trace_id,
                pid: event.host_pid,
                generation: event.generation,
                operation,
                fd,
                dirfd: None,
                target_dirfd: None,
                target_fd: None,
                last_fd: None,
                result: event.result,
                raw_path: None,
                raw_path2: None,
            });
            let mut metadata = BTreeMap::new();
            metadata.insert("fd".to_string(), fd.to_string());
            metadata.insert("result".to_string(), event.result.to_string());
            metadata.insert("path_resolution".to_string(), resolution.source.to_string());
            if let Some(path) = resolution.target_path {
                metadata.insert("path2".to_string(), path);
            }
            output.push(RawCollectorEvent {
                envelope: envelope(observed_at, &process),
                payload: RawObservationPayload::File {
                    operation: operation.to_string(),
                    path: resolution.path,
                    fd: Some(fd as i32),
                    size: Some(event.requested_size),
                    metadata,
                },
            });
        }
        output
    }

    fn decode_batch(&mut self, events: Vec<KernelProcessEvent>) -> Vec<RawCollectorEvent> {
        let mut output = Vec::new();
        for event in events {
            let observed_at = event_observed_at(&event);
            if event.kind == EVENT_FD_IO {
                output.extend(self.fan_out_fd_io(event));
                continue;
            }
            if event.kind == EVENT_TLS {
                let process = self.observation(event.host_pid, event.generation);
                let direction = if event.tls_direction == Some(1) {
                    model_core::payload::PayloadDirection::Inbound
                } else {
                    model_core::payload::PayloadDirection::Outbound
                };
                let direction_code = event.tls_direction.unwrap_or(0);
                let sequence_key = (
                    event.host_pid,
                    event.generation,
                    event.tls_connection,
                    direction_code,
                );
                let sequence = self.tls_sequences.entry(sequence_key).or_insert(0);
                let segment_sequence = *sequence;
                *sequence = sequence.saturating_add(1);
                let bytes = event.tls_payload.clone();
                let captured_size = bytes.as_ref().map_or(0, |value| value.len() as u64);
                let chunk_declared_size = event
                    .requested_size
                    .saturating_sub(event.tls_chunk_offset)
                    .min(TLS_PAYLOAD_MAX as u64)
                    .min(captured_size);
                let mut metadata = BTreeMap::new();
                metadata.insert(
                    "direction".to_string(),
                    if event.tls_direction == Some(1) {
                        "inbound"
                    } else {
                        "outbound"
                    }
                    .to_string(),
                );
                metadata.insert(
                    "connection_ptr".to_string(),
                    format!("0x{:x}", event.tls_connection),
                );
                metadata.insert("call_id".to_string(), event.tls_call_id.to_string());
                metadata.insert("chunk_index".to_string(), event.tls_chunk_index.to_string());
                metadata.insert(
                    "chunk_offset".to_string(),
                    event.tls_chunk_offset.to_string(),
                );
                metadata.insert(
                    "requested_size".to_string(),
                    event.requested_size.to_string(),
                );
                metadata.insert(
                    "symbol".to_string(),
                    match event.tls_symbol {
                        Some(1) => "SSL_write",
                        Some(2) => "SSL_read",
                        Some(11) => "SSL_write_ex",
                        Some(12) => "SSL_read_ex",
                        Some(3) => "gnutls_record_send",
                        Some(4) => "gnutls_record_recv",
                        Some(5) => "PR_Write",
                        Some(6) => "PR_Read",
                        Some(7) => "crypto/tls.(*Conn).Write",
                        Some(8) => "crypto/tls.(*Conn).Read",
                        Some(9) => "rustls_write_tls",
                        Some(10) => "rustls_read_tls",
                        _ => "unknown",
                    }
                    .to_string(),
                );
                if event.tls_flags != 0 {
                    metadata.insert("capture_gap".to_string(), "true".to_string());
                }
                self.payload_segments.push(RawPayloadSegment {
                    envelope: RawEventEnvelope {
                        trace_id: Some(event.trace_id),
                        observed_at,
                        process: process.clone(),
                        collector: CollectorName::new("ebpf"),
                        session_id: Some(event.session_id.clone()).flatten(),
                        call_id: event.call_id.clone(),
                    },
                    source: model_core::payload::PayloadSourceBoundary::Uprobe,
                    content_state: if event.tls_flags & 1 != 0 {
                        model_core::payload::PayloadContentState::Loss
                    } else if event.tls_flags & 2 != 0 {
                        model_core::payload::PayloadContentState::Truncated
                    } else {
                        model_core::payload::PayloadContentState::Complete
                    },
                    direction,
                    stream_key: Some(format!(
                        "tls:{}:{}:{}",
                        event.host_pid, event.tls_connection, direction_code
                    )),
                    sequence: segment_sequence,
                    operation_id: Some(event.tls_call_id),
                    offset: Some(event.tls_chunk_offset),
                    completed: event.tls_chunk_flags & 8 != 0,
                    original_size: chunk_declared_size,
                    captured_size,
                    library: Some(
                        match event.tls_symbol {
                            Some(3 | 4) => "gnutls",
                            Some(5 | 6) => "nss",
                            Some(7 | 8) => "go_tls",
                            Some(9 | 10) => "rustls",
                            _ => "openssl",
                        }
                        .to_string(),
                    ),
                    symbol: metadata.get("symbol").cloned(),
                    protocol_hint: Some("tls".to_string()),
                    loss_reason: (event.tls_flags != 0)
                        .then(|| "TLS uprobe capture gap".to_string()),
                    bytes,
                });
                continue;
            }
            if event.kind == EVENT_NET {
                let operation = net_operation(event.reserved);
                let process = self.observation(event.host_pid, event.generation);
                let fd_key = (event.host_pid, event.generation, event.fd);
                if is_fd_net_operation(event.reserved) && self.ipc_unix_fds.contains(&fd_key) {
                    continue;
                }
                if is_fd_net_operation(event.reserved)
                    && !self.socket_fds.contains(&fd_key)
                    && !fd_is_socket(event.host_pid, event.fd)
                {
                    continue;
                }
                self.socket_fds.insert(fd_key);
                let mut metadata = BTreeMap::new();
                metadata.insert("fd".to_string(), event.fd.to_string());
                let endpoint_decoded = event.net_endpoint.as_deref().and_then(decode_endpoint);
                if endpoint_decoded.is_some() {
                    if let Some(endpoint) = &endpoint_decoded {
                        metadata.insert("endpoint_family".to_string(), endpoint.0.clone());
                        metadata.insert(
                            "endpoint_role".to_string(),
                            if event.reserved == 5 {
                                "local"
                            } else {
                                "remote"
                            }
                            .to_string(),
                        );
                    }
                }
                if event.net_endpoint_loss {
                    metadata.insert("endpoint_capture_gap".to_string(), "true".to_string());
                }
                if event.net_endpoint.is_some() && endpoint_decoded.is_none() {
                    metadata.insert("endpoint_capture_gap".to_string(), "true".to_string());
                }
                output.push(RawCollectorEvent {
                    envelope: RawEventEnvelope {
                        trace_id: Some(event.trace_id),
                        observed_at,
                        process,
                        collector: CollectorName::new("ebpf"),
                        session_id: Some(event.session_id.clone()).flatten(),
                        call_id: event.call_id.clone(),
                    },
                    payload: RawObservationPayload::Net {
                        operation: operation.to_string(),
                        endpoint: event
                            .net_endpoint
                            .as_deref()
                            .and_then(|bytes| decode_endpoint(bytes).map(|(_, value)| value)),
                        direction: net_direction(event.reserved),
                        length: net_length(event.reserved, event.requested_size, event.result),
                        result: Some(i64::from(event.result)),
                        metadata,
                    },
                });
                continue;
            }
            if event.kind == EVENT_FILE {
                let operation = match event.reserved {
                    1 => "open",
                    2 => "openat",
                    3 => "creat",
                    4 => "unlinkat",
                    5 => "renameat",
                    6 => "mkdirat",
                    7 => "mmap",
                    8 => "close",
                    9 => "close_range",
                    10 => "dup",
                    11 => "dup2",
                    12 => "dup3",
                    13 => "fcntl",
                    14 => "chdir",
                    15 => "fchdir",
                    16 => "read",
                    17 => "write",
                    18 => "readv",
                    19 => "writev",
                    20 => "rmdir",
                    21 => "openat2",
                    22 => "truncate",
                    23 => "ftruncate",
                    24 => "renameat2",
                    25 => "rename",
                    26 => "unlink",
                    27 => "mkdir",
                    _ => "unknown",
                };
                let process = self.observation(event.host_pid, event.generation);
                if event.reserved == 7
                    && event.result >= 0
                    && event.fd != u32::MAX
                    && self.config.tls_dynamic_loading
                {
                    let already_seen_image =
                        if matches!(self.config.tls_scan_debounce, TlsScanDebounce::Image) {
                            std::fs::read_link(format!("/proc/{}/fd/{}", event.host_pid, event.fd))
                                .ok()
                                .map(|path| {
                                    let mut image_path = path.to_string_lossy().into_owned();
                                    if let Some(length) =
                                        image_path.strip_suffix(" (deleted)").map(str::len)
                                    {
                                        image_path.truncate(length);
                                    }
                                    let key = (
                                        event.trace_id,
                                        event.host_pid,
                                        event.generation,
                                        image_path,
                                    );
                                    let previous = self.tls_image_scans.insert(key, observed_at);
                                    previous.is_some_and(|last| {
                                        observed_at.duration_since(last).unwrap_or_default()
                                            < Duration::from_millis(
                                                self.config.tls_scan_interval_ms,
                                            )
                                    })
                                })
                                .unwrap_or(false)
                        } else {
                            false
                        };
                    if !already_seen_image {
                        self.refresh_tls_for_process(event.trace_id, &process, "mmap");
                    }
                }
                let mut metadata = BTreeMap::new();
                metadata.insert("fd".to_string(), event.fd.to_string());
                metadata.insert("result".to_string(), event.result.to_string());
                match event.reserved {
                    10..=12 if event.result >= 0 => {
                        metadata.insert(
                            "target_fd".to_string(),
                            if event.reserved == 10 {
                                event.result.to_string()
                            } else {
                                event.requested_size.to_string()
                            },
                        );
                    }
                    13 => {
                        metadata.insert("command".to_string(), event.requested_size.to_string());
                        if matches!(event.requested_size, 0 | 1030) && event.result >= 0 {
                            metadata.insert("target_fd".to_string(), event.result.to_string());
                        }
                    }
                    _ => {}
                }
                let fd_key = (event.host_pid, event.generation, event.fd);
                let socket_io = matches!(event.reserved, 16..=19)
                    && !self.file_fds.contains(&fd_key)
                    && (self.socket_fds.contains(&fd_key)
                        || fd_is_socket(event.host_pid, event.fd));
                match event.reserved {
                    1 | 2 | 3 | 21 if event.result >= 0 => {
                        // open/openat/creat/openat2 success returns a regular
                        // file: record it (dropping stale classifications) so
                        // fd-io fan-out never probes /proc at decode time.
                        self.file_fds.insert(fd_key);
                        self.socket_fds.remove(&fd_key);
                        self.ipc_unix_fds.remove(&fd_key);
                    }
                    8 if event.result >= 0 => {
                        self.socket_fds.remove(&fd_key);
                        self.ipc_unix_fds.remove(&fd_key);
                        self.file_fds.remove(&fd_key);
                    }
                    9 if event.result >= 0 => {
                        let last_fd = u32::try_from(event.requested_size).unwrap_or(u32::MAX);
                        self.socket_fds.retain(|(pid, generation, fd)| {
                            *pid != event.host_pid
                                || *generation != event.generation
                                || *fd < event.fd
                                || *fd > last_fd
                        });
                        self.ipc_unix_fds.retain(|(pid, generation, fd)| {
                            *pid != event.host_pid
                                || *generation != event.generation
                                || *fd < event.fd
                                || *fd > last_fd
                        });
                        self.file_fds.retain(|(pid, generation, fd)| {
                            *pid != event.host_pid
                                || *generation != event.generation
                                || *fd < event.fd
                                || *fd > last_fd
                        });
                    }
                    10..=13 if event.result >= 0 => {
                        if let Some(target) = metadata
                            .get("target_fd")
                            .and_then(|value| value.parse::<u32>().ok())
                        {
                            let target_key = (event.host_pid, event.generation, target);
                            if self.socket_fds.contains(&fd_key)
                                || fd_is_socket(event.host_pid, event.fd)
                            {
                                self.socket_fds.insert(target_key);
                            }
                            if self.ipc_unix_fds.contains(&fd_key) {
                                self.ipc_unix_fds.insert(target_key);
                            }
                            if self.file_fds.contains(&fd_key) {
                                self.file_fds.insert(target_key);
                            }
                        }
                    }
                    _ => {}
                }
                if socket_io {
                    continue;
                }
                if event.file_path_truncated {
                    metadata.insert("path_truncated".to_string(), "true".to_string());
                }
                if event.file_path_capture_gap {
                    metadata.insert("path_capture_gap".to_string(), "true".to_string());
                }
                let resolution = self.path_state.observe(FileObservation {
                    trace_id: event.trace_id,
                    pid: event.host_pid,
                    generation: event.generation,
                    operation,
                    fd: event.fd,
                    dirfd: file_dirfd(event.reserved, event.aux_fd),
                    target_dirfd: (event.reserved == 5 || event.reserved == 24)
                        .then_some(event.requested_size as i32),
                    target_fd: metadata
                        .get("target_fd")
                        .and_then(|value| value.parse::<u32>().ok()),
                    last_fd: (event.reserved == 9)
                        .then(|| u32::try_from(event.requested_size).unwrap_or(u32::MAX)),
                    result: event.result,
                    raw_path: event.file_path.as_deref(),
                    raw_path2: event.file_path2.as_deref(),
                });
                if let Some(raw_path) = &event.file_path {
                    metadata.insert("raw_path".to_string(), raw_path.clone());
                }
                if let Some(raw_path2) = &event.file_path2 {
                    metadata.insert("raw_path2".to_string(), raw_path2.clone());
                }
                metadata.insert("path_resolution".to_string(), resolution.source.to_string());
                if event.file_path2.is_some() {
                    metadata.insert(
                        "target_path_resolution".to_string(),
                        resolution.target_source.to_string(),
                    );
                }
                if let Some(path2) = &resolution.target_path {
                    metadata.insert("path2".to_string(), path2.clone());
                }
                output.push(RawCollectorEvent {
                    envelope: RawEventEnvelope {
                        trace_id: Some(event.trace_id),
                        observed_at,
                        process,
                        collector: CollectorName::new("ebpf"),
                        session_id: Some(event.session_id.clone()).flatten(),
                        call_id: event.call_id.clone(),
                    },
                    payload: RawObservationPayload::File {
                        operation: operation.to_string(),
                        path: resolution.path,
                        fd: Some(event.fd as i32),
                        size: Some(event.requested_size),
                        metadata,
                    },
                });
                continue;
            }
            if event.kind == EVENT_IPC {
                let operation = match event.reserved {
                    1 => "pipe",
                    2 => "socketpair",
                    _ => "unknown",
                };
                let process = self.observation(event.host_pid, event.generation);
                if event.reserved == 2 {
                    self.ipc_unix_fds
                        .insert((event.host_pid, event.generation, event.fd));
                    self.ipc_unix_fds
                        .insert((event.host_pid, event.generation, event.aux_fd));
                }
                let mut metadata = BTreeMap::new();
                metadata.insert("fd_a".to_string(), event.fd.to_string());
                metadata.insert("fd_b".to_string(), event.aux_fd.to_string());
                metadata.insert("domain".to_string(), event.requested_size.to_string());
                output.push(RawCollectorEvent {
                    envelope: RawEventEnvelope {
                        trace_id: Some(event.trace_id),
                        observed_at,
                        process,
                        collector: CollectorName::new("ebpf"),
                        session_id: Some(event.session_id.clone()).flatten(),
                        call_id: event.call_id.clone(),
                    },
                    payload: RawObservationPayload::Ipc {
                        operation: operation.to_string(),
                        channel: Some(operation.to_string()),
                        direction: model_core::payload::PayloadDirection::Bidirectional,
                        length: None,
                        metadata,
                    },
                });
                continue;
            }
            let (operation, process, parent, metadata, retire) = match event.kind {
                EVENT_FORK => {
                    let parent = self.observation(event.host_pid, event.generation);
                    let child = self.observation(event.child_host_pid, event.child_generation);
                    let inherited_socket_fds = self
                        .socket_fds
                        .iter()
                        .filter(|(pid, generation, _)| {
                            *pid == event.host_pid && *generation == event.generation
                        })
                        .map(|(_, _, fd)| (event.child_host_pid, event.child_generation, *fd))
                        .collect::<Vec<_>>();
                    self.socket_fds.extend(inherited_socket_fds);
                    let inherited_ipc_unix_fds = self
                        .ipc_unix_fds
                        .iter()
                        .filter(|(pid, generation, _)| {
                            *pid == event.host_pid && *generation == event.generation
                        })
                        .map(|(_, _, fd)| (event.child_host_pid, event.child_generation, *fd))
                        .collect::<Vec<_>>();
                    self.ipc_unix_fds.extend(inherited_ipc_unix_fds);
                    let inherited_file_fds = self
                        .file_fds
                        .iter()
                        .filter(|(pid, generation, _)| {
                            *pid == event.host_pid && *generation == event.generation
                        })
                        .map(|(_, _, fd)| (event.child_host_pid, event.child_generation, *fd))
                        .collect::<Vec<_>>();
                    self.file_fds.extend(inherited_file_fds);
                    self.path_state.fork(
                        event.trace_id,
                        event.host_pid,
                        event.generation,
                        event.child_host_pid,
                        event.child_generation,
                    );
                    self.processes
                        .insert(event.child_host_pid, (event.trace_id, child.clone()));
                    self.trace_pids
                        .entry(event.trace_id)
                        .or_default()
                        .insert(event.child_host_pid);
                    self.refresh_tls_for_process(event.trace_id, &child, "fork");
                    ("fork", child, Some(parent), BTreeMap::new(), false)
                }
                EVENT_EXEC => {
                    let process = self.observation(event.host_pid, event.generation);
                    let mut metadata = BTreeMap::new();
                    if let Some(executable) = event.executable {
                        metadata.insert("executable".to_string(), executable);
                    }
                    if let Some(runtime) = &mut self.runtime {
                        runtime.detach_tls_process(
                            event.trace_id,
                            event.host_pid,
                            event.generation,
                        );
                    }
                    self.refresh_tls_for_process(event.trace_id, &process, "exec");
                    ("exec", process, None, metadata, false)
                }
                EVENT_EXIT => {
                    let process = self.observation(event.host_pid, event.generation);
                    let mut metadata = BTreeMap::new();
                    if event.result != 0 {
                        metadata
                            .insert("exit_code".to_string(), (event.reserved as i32).to_string());
                    }
                    ("exit", process, None, metadata, true)
                }
                EVENT_SIGNAL => {
                    let process = self.observation(event.host_pid, event.generation);
                    let mut metadata = BTreeMap::new();
                    metadata.insert("signal".to_string(), event.fd.to_string());
                    metadata.insert("target_pid".to_string(), event.requested_size.to_string());
                    metadata.insert("result".to_string(), event.result.to_string());
                    if event.aux_fd != 0 {
                        metadata.insert("target_group".to_string(), event.aux_fd.to_string());
                    }
                    ("signal", process, None, metadata, false)
                }
                _ => continue,
            };
            output.push(RawCollectorEvent {
                envelope: RawEventEnvelope {
                    trace_id: Some(event.trace_id),
                    observed_at,
                    process,
                    collector: CollectorName::new("ebpf"),
                    session_id: Some(event.session_id.clone()).flatten(),
                    call_id: event.call_id.clone(),
                },
                payload: RawObservationPayload::Process {
                    operation: operation.to_string(),
                    parent,
                    argv: event.argv,
                    metadata,
                },
            });
            if retire {
                self.socket_fds.retain(|(pid, generation, _)| {
                    *pid != event.host_pid || *generation != event.generation
                });
                self.ipc_unix_fds.retain(|(pid, generation, _)| {
                    *pid != event.host_pid || *generation != event.generation
                });
                self.file_fds.retain(|(pid, generation, _)| {
                    *pid != event.host_pid || *generation != event.generation
                });
                self.stdio_sequences
                    .retain(|(pid, _), _| *pid != event.host_pid);
                self.path_state
                    .release(event.trace_id, event.host_pid, event.generation);
                self.tls_last_refresh
                    .remove(&(event.trace_id, event.host_pid, event.generation));
                self.tls_image_scans
                    .retain(|(trace, pid, generation, _), _| {
                        *trace != event.trace_id
                            || *pid != event.host_pid
                            || *generation != event.generation
                    });
                if let Some(runtime) = &mut self.runtime {
                    runtime.detach_tls_process(event.trace_id, event.host_pid, event.generation);
                }
                self.processes.remove(&event.host_pid);
                if let Some(pids) = self.trace_pids.get_mut(&event.trace_id) {
                    pids.remove(&event.host_pid);
                }
            }
        }
        output
    }
}

impl CollectorInstance for EbpfCollector {
    fn descriptor(&self) -> &collector_capability::CollectorDescriptor {
        &self.probe_result.descriptor
    }

    fn bind_trace(
        &mut self,
        request: &TraceBindingRequest,
    ) -> Result<TraceBindingHandle, CollectorError> {
        self.requested_capabilities.extend(
            request
                .requested_capabilities
                .iter()
                .map(|item| item.capability),
        );
        self.track_observation(request.trace_id, request.root_observation.clone())?;
        self.refresh_tls_for_process(request.trace_id, &request.root_observation, "attach");
        Ok(TraceBindingHandle {
            collector: self.probe_result.descriptor.clone(),
            bound_at: SystemTime::now(),
        })
    }

    fn unbind_trace(&mut self, trace_id: TraceId) -> Result<(), CollectorError> {
        if let Some(runtime) = &mut self.runtime {
            runtime.detach_tls_trace(trace_id);
        }
        let pids = self.trace_pids.remove(&trace_id).unwrap_or_default();
        for pid in pids {
            if let Some(runtime) = &self.runtime {
                runtime.untrack(pid).map_err(collector_error)?;
            }
            self.processes.remove(&pid);
            self.socket_fds
                .retain(|(tracked_pid, _, _)| *tracked_pid != pid);
            self.ipc_unix_fds
                .retain(|(tracked_pid, _, _)| *tracked_pid != pid);
            self.file_fds
                .retain(|(tracked_pid, _, _)| *tracked_pid != pid);
            self.stdio_sequences
                .retain(|(tracked_pid, _), _| *tracked_pid != pid);
            self.path_state.release_pid(trace_id, pid);
            self.tls_last_refresh
                .retain(|(tracked_trace, tracked_pid, _), _| {
                    *tracked_trace != trace_id || *tracked_pid != pid
                });
            self.tls_image_scans
                .retain(|(tracked_trace, tracked_pid, _, _), _| {
                    *tracked_trace != trace_id || *tracked_pid != pid
                });
        }
        Ok(())
    }

    fn poll_batch(&mut self) -> Result<CollectorPollBatch, CollectorError> {
        let events = match &mut self.runtime {
            Some(runtime) => runtime.poll().map_err(collector_error)?,
            None => Vec::new(),
        };
        let mut observations = std::mem::take(&mut self.pending_tls_diagnostics);
        observations.extend(self.decode_batch(events));
        Ok(CollectorPollBatch {
            observations,
            payload_segments: std::mem::take(&mut self.payload_segments),
        })
    }

    fn poll_raw_batch(&mut self) -> Result<Option<CollectorRawBatch>, CollectorError> {
        let raws = match &mut self.runtime {
            Some(runtime) => runtime.poll_raw().map_err(collector_error)?,
            None => Vec::new(),
        };
        let diagnostics = std::mem::take(&mut self.pending_tls_diagnostics);
        if raws.is_empty() && diagnostics.is_empty() {
            return Ok(None);
        }
        let mut lifecycle = Vec::new();
        for raw in &raws {
            let kind = raw
                .get(..4)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u32::from_ne_bytes);
            if matches!(kind, Some(EVENT_FORK) | Some(EVENT_EXEC) | Some(EVENT_EXIT)) {
                if let Ok(event) = decode_event(raw) {
                    lifecycle.push(event);
                }
            }
        }
        if !lifecycle.is_empty() {
            let _ = self.decode_batch(lifecycle);
        }
        Ok(Some(CollectorRawBatch {
            events: raws,
            diagnostics,
        }))
    }

    fn transport_fd(&self) -> Option<std::os::fd::RawFd> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.event_buffer.epoll_fd())
    }

    fn flush_transport(&mut self) -> Result<(), CollectorError> {
        if let Some(runtime) = &mut self.runtime {
            runtime
                .event_buffer
                .consume()
                .map_err(|error| CollectorError::new("flush_bpf", error.to_string()))?;
        }
        Ok(())
    }

    fn stats(&self) -> CollectorStats {
        let mut dropped = Vec::new();
        if let Some(runtime) = self.runtime.as_ref() {
            let decode = runtime.decode_failures();
            if decode > 0 {
                dropped.push(collector_stats::DropCounter {
                    reason: "decode_failure".to_string(),
                    count: decode,
                });
            }
            for (class, count) in runtime.loss_counters() {
                dropped.push(collector_stats::DropCounter {
                    reason: class.to_string(),
                    count,
                });
            }
        }
        CollectorStats {
            collector_name: CollectorName::new("ebpf"),
            active_bindings: self.trace_pids.len(),
            last_heartbeat_at: SystemTime::now(),
            dropped,
        }
    }
}

fn decode_event(raw: &[u8]) -> Result<KernelProcessEvent, LoaderError> {
    if raw.len() != PROCESS_EVENT_SIZE
        && raw.len() != EXEC_EVENT_SIZE
        && raw.len() != FILE_EVENT_SIZE
        && raw.len() != NET_EVENT_SIZE
        && raw.len() != TLS_EVENT_SIZE
        && raw.len() != FD_IO_EVENT_SIZE
    {
        return Err(LoaderError::new(
            "decode_bpf",
            format!("unexpected event size {}", raw.len()),
        ));
    }
    let kind = read_u32(raw, 0)?;
    let executable = if kind == EVENT_EXEC && raw.len() == EXEC_EVENT_SIZE {
        let size = usize::try_from(read_u32(raw, PROCESS_EVENT_SIZE)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        if size > EXEC_FILENAME_MAX {
            return Err(LoaderError::new("decode_bpf", "exec filename is too large"));
        }
        (size > 0).then(|| {
            String::from_utf8_lossy(&raw[PROCESS_EVENT_SIZE + 8..PROCESS_EVENT_SIZE + 8 + size])
                .into_owned()
        })
    } else {
        None
    };
    let argv = if kind == EVENT_EXEC && raw.len() == EXEC_EVENT_SIZE {
        let base = PROCESS_EVENT_SIZE + 8 + EXEC_FILENAME_MAX;
        let count = usize::try_from(read_u32(raw, base)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        let flags = read_u32(raw, base + 4)?;
        let bytes_size = usize::try_from(read_u32(raw, base + 8)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        if count > EXEC_ARG_MAX || bytes_size > EXEC_ARG_BYTES_MAX {
            return Err(LoaderError::new(
                "decode_bpf",
                "argv capture exceeds ABI limits",
            ));
        }
        let entries_base = base + 16;
        let bytes_base = entries_base + EXEC_ARG_MAX * 8;
        let mut args = Vec::with_capacity(count);
        for index in 0..count {
            let entry = entries_base + index * 8;
            let offset = usize::try_from(read_u32(raw, entry)?)
                .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
            let length = usize::try_from(read_u32(raw, entry + 4)?)
                .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
            if offset
                .checked_add(length)
                .is_none_or(|end| end > bytes_size)
            {
                return Err(LoaderError::new(
                    "decode_bpf",
                    "argv entry outside captured bytes",
                ));
            }
            args.push(raw[bytes_base + offset..bytes_base + offset + length].to_vec());
        }
        Some(ArgvCapture { args, flags })
    } else {
        None
    };
    let (file_path, file_path2, file_path_truncated, file_path_capture_gap) = if kind == EVENT_FILE
        && raw.len() == FILE_EVENT_SIZE
    {
        let path_size = usize::try_from(read_u32(raw, PROCESS_EVENT_SIZE)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        let path_flags = read_u32(raw, PROCESS_EVENT_SIZE + 4)?;
        let path2_size = usize::try_from(read_u32(raw, PROCESS_EVENT_SIZE + 8)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        let path2_flags = read_u32(raw, PROCESS_EVENT_SIZE + 12)?;
        if path_size > FILE_PATH_MAX || path2_size > FILE_PATH_MAX {
            return Err(LoaderError::new("decode_bpf", "file path is too large"));
        }
        let path_base = PROCESS_EVENT_SIZE + 16;
        let path = (path_size > 0)
            .then(|| String::from_utf8_lossy(&raw[path_base..path_base + path_size]).into_owned());
        let path2_base = path_base + FILE_PATH_MAX;
        let path2 = (path2_size > 0).then(|| {
            String::from_utf8_lossy(&raw[path2_base..path2_base + path2_size]).into_owned()
        });
        (
            path,
            path2,
            (path_flags & 1) != 0 || (path2_flags & 1) != 0,
            (path_flags & 2) != 0 || (path2_flags & 2) != 0,
        )
    } else {
        (None, None, false, false)
    };
    let (net_endpoint, net_endpoint_loss) = if kind == EVENT_NET && raw.len() == NET_EVENT_SIZE {
        let size = usize::try_from(read_u32(raw, PROCESS_EVENT_SIZE)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        if size > NET_ENDPOINT_MAX {
            return Err(LoaderError::new(
                "decode_bpf",
                "network endpoint is too large",
            ));
        }
        let flags = read_u32(raw, PROCESS_EVENT_SIZE + 4)?;
        let base = PROCESS_EVENT_SIZE + 8;
        (
            (size > 0).then(|| raw[base..base + size].to_vec()),
            flags != 0,
        )
    } else {
        (None, false)
    };
    let (
        tls_direction,
        tls_symbol,
        tls_payload,
        tls_flags,
        tls_connection,
        tls_call_id,
        tls_chunk_offset,
        tls_chunk_index,
        tls_chunk_flags,
    ) = if kind == EVENT_TLS && raw.len() == TLS_EVENT_SIZE {
        let size = usize::try_from(read_u32(raw, PROCESS_EVENT_SIZE)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        let flags = read_u32(raw, PROCESS_EVENT_SIZE + 4)?;
        let direction = read_u32(raw, PROCESS_EVENT_SIZE + 8)?;
        let symbol = read_u32(raw, PROCESS_EVENT_SIZE + 12)?;
        let connection = read_u64(raw, PROCESS_EVENT_SIZE + 16)?;
        let call_id = read_u64(raw, PROCESS_EVENT_SIZE + 24)?;
        let chunk_offset = read_u64(raw, PROCESS_EVENT_SIZE + 32)?;
        let chunk_index = read_u32(raw, PROCESS_EVENT_SIZE + 40)?;
        let chunk_flags = read_u32(raw, PROCESS_EVENT_SIZE + 44)?;
        if size > TLS_PAYLOAD_MAX {
            return Err(LoaderError::new("decode_bpf", "TLS payload is too large"));
        }
        (
            Some(direction),
            Some(symbol),
            (size > 0)
                .then(|| raw[PROCESS_EVENT_SIZE + 48..PROCESS_EVENT_SIZE + 48 + size].to_vec()),
            flags,
            connection,
            call_id,
            chunk_offset,
            chunk_index,
            chunk_flags,
        )
    } else {
        (None, None, None, 0, 0, 0, 0, 0, 0)
    };
    let (fd_io_operation, fd_io_payload, fd_io_loss) = if kind == EVENT_FD_IO
        && raw.len() == FD_IO_EVENT_SIZE
    {
        let size = usize::try_from(read_u32(raw, PROCESS_EVENT_SIZE)?)
            .map_err(|error| LoaderError::new("decode_bpf", error.to_string()))?;
        if size > FD_IO_PAYLOAD_MAX {
            return Err(LoaderError::new("decode_bpf", "fd-io payload is too large"));
        }
        let flags = read_u32(raw, PROCESS_EVENT_SIZE + 4)?;
        (
            Some(read_u32(raw, 8)?),
            (size > 0).then(|| raw[PROCESS_EVENT_SIZE + 8..PROCESS_EVENT_SIZE + 8 + size].to_vec()),
            flags != 0,
        )
    } else {
        (None, None, false)
    };
    Ok(KernelProcessEvent {
        kind,
        host_pid: read_u32(raw, 12)?,
        child_host_pid: read_u32(raw, 16)?,
        result: read_i32(raw, 20)?,
        trace_id: TraceId::new(read_u64(raw, 24)?),
        observed_ktime_ns: read_u64(raw, 32)?,
        reserved: read_u32(raw, 8)?,
        generation: read_u64(raw, 56)?,
        child_generation: read_u64(raw, 64)?,
        executable,
        argv,
        fd: read_u32(raw, 40)?,
        aux_fd: read_u32(raw, 44)?,
        requested_size: read_u64(raw, 48)?,
        file_path,
        file_path2,
        file_path_truncated,
        file_path_capture_gap,
        net_endpoint,
        net_endpoint_loss,
        tls_direction,
        tls_symbol,
        tls_payload,
        tls_flags,
        tls_connection,
        tls_call_id,
        tls_chunk_offset,
        tls_chunk_index,
        tls_chunk_flags,
        fd_io_operation,
        fd_io_payload,
        fd_io_loss,
        session_id: parse_session(&raw[72..72 + SESSION_LEN]),
        call_id: parse_string(&raw[72 + SESSION_LEN..PROCESS_EVENT_SIZE]),
    })
}

fn parse_session(bytes: &[u8]) -> Option<model_core::process::SessionIdentity> {
    parse_string(bytes).map(model_core::process::SessionIdentity::new)
}

fn parse_string(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    (end > 0).then(|| String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn decode_endpoint(bytes: &[u8]) -> Option<(String, String)> {
    if bytes.len() < 2 {
        return None;
    }
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]);
    match family {
        2 if bytes.len() >= 8 => {
            let port = u16::from_be_bytes([bytes[2], bytes[3]]);
            let address = Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]);
            Some(("ipv4".to_string(), format!("{address}:{port}")))
        }
        10 if bytes.len() >= 24 => {
            let port = u16::from_be_bytes([bytes[2], bytes[3]]);
            let mut address = [0_u8; 16];
            address.copy_from_slice(&bytes[8..24]);
            Some((
                "ipv6".to_string(),
                format!("[{}]:{port}", Ipv6Addr::from(address)),
            ))
        }
        1 => {
            let end = bytes[2..]
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(bytes.len() - 2);
            let path = String::from_utf8_lossy(&bytes[2..2 + end]).into_owned();
            Some(("unix".to_string(), path))
        }
        _ => None,
    }
}

fn net_operation(operation: u32) -> &'static str {
    match operation {
        1 => "connect",
        2 => "accept",
        3 => "send",
        4 => "recv",
        5 => "bind",
        6 => "listen",
        7 => "sendmsg",
        8 => "write",
        9 => "read",
        10 => "writev",
        11 => "readv",
        _ => "unknown",
    }
}

fn file_dirfd(operation: u32, raw: u32) -> Option<i32> {
    matches!(operation, 2 | 4 | 5 | 6 | 21 | 24).then_some(raw as i32)
}

fn net_direction(operation: u32) -> model_core::payload::PayloadDirection {
    match operation {
        3 | 7 | 8 | 10 => model_core::payload::PayloadDirection::Outbound,
        4 | 9 | 11 => model_core::payload::PayloadDirection::Inbound,
        _ => model_core::payload::PayloadDirection::Unknown,
    }
}

fn net_length(operation: u32, requested_size: u64, result: i32) -> Option<u64> {
    if matches!(operation, 7..=11) {
        u64::try_from(result).ok()
    } else {
        Some(requested_size)
    }
}

fn is_fd_net_operation(operation: u32) -> bool {
    matches!(operation, 8..=11)
}

fn fd_is_socket(pid: u32, fd: u32) -> bool {
    fd_probe_kind(pid, fd) == Some(true)
}

/// Live fd-kind probe.  `Some(true)` = socket, `Some(false)` = determinable
/// non-socket (regular file, pipe, pty…), `None` = cannot determine (the
/// process or fd is gone, or /proc is unreadable).  The answer reflects the
/// fd's state at decode time, so callers must prefer capture-order knowledge
/// (e.g. an observed openat/close) over this probe.
fn fd_probe_kind(pid: u32, fd: u32) -> Option<bool> {
    std::fs::read_link(format!("/proc/{pid}/fd/{fd}"))
        .ok()
        .map(|target| {
            target
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(b"socket:[")
        })
}

/// Decide whether an fd-io record belongs to the socket class.  Capture-order
/// file knowledge always wins: once an fd is observed as a regular file, a
/// recycled fd number must not flip its later fd-io into the net class, no
/// matter what the live probe currently reports.
fn fd_is_socket_class(known_socket: bool, known_file: bool, probed: Option<bool>) -> bool {
    if known_file {
        return false;
    }
    if known_socket {
        return true;
    }
    probed == Some(true)
}

fn read_u32(raw: &[u8], offset: usize) -> Result<u32, LoaderError> {
    raw.get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_ne_bytes)
        .ok_or_else(|| LoaderError::new("decode_bpf", "truncated u32"))
}

fn read_i32(raw: &[u8], offset: usize) -> Result<i32, LoaderError> {
    raw.get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(i32::from_ne_bytes)
        .ok_or_else(|| LoaderError::new("decode_bpf", "truncated i32"))
}

fn read_u64(raw: &[u8], offset: usize) -> Result<u64, LoaderError> {
    raw.get(offset..offset + 8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_ne_bytes)
        .ok_or_else(|| LoaderError::new("decode_bpf", "truncated u64"))
}

fn resize_map(
    open: &mut libbpf_rs::OpenObject,
    name: &str,
    max_entries: u32,
) -> Result<(), LoaderError> {
    open.maps_mut()
        .find(|map| map.name() == OsStr::new(name))
        .ok_or_else(|| LoaderError::new("resize_bpf_map", format!("missing map {name}")))?
        .set_max_entries(max_entries)
        .map_err(|error| {
            LoaderError::new(
                "resize_bpf_map",
                format!("map {name} max_entries={max_entries}: {error}"),
            )
        })
}

fn map_handle(object: &Object, name: &str) -> Result<MapHandle, LoaderError> {
    let map = object
        .maps()
        .find(|map| map.name() == OsStr::new(name))
        .ok_or_else(|| LoaderError::new("bpf_map", format!("missing map {name}")))?;
    MapHandle::try_from(&map).map_err(|error| LoaderError::new("bpf_map", error.to_string()))
}

fn apply_memlock_rlimit(limit: MemlockRlimit) -> Result<(), LoaderError> {
    let value = match limit {
        MemlockRlimit::Inherit => return Ok(()),
        MemlockRlimit::Unlimited => libc::rlimit {
            rlim_cur: libc::RLIM_INFINITY,
            rlim_max: libc::RLIM_INFINITY,
        },
        MemlockRlimit::Bytes(bytes) => libc::rlimit {
            rlim_cur: bytes,
            rlim_max: bytes,
        },
    };
    // SAFETY: `value` is fully initialized and lives for the duration of the
    // syscall; `setrlimit` does not retain the pointer.
    let result = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &value) };
    if result == 0 {
        Ok(())
    } else {
        Err(LoaderError::new(
            "memlock",
            std::io::Error::last_os_error().to_string(),
        ))
    }
}

fn collector_error(error: LoaderError) -> CollectorError {
    CollectorError::new(error.stage, error.message)
}

fn _system_time_from_ns(ns: u64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_nanos(ns)
}
