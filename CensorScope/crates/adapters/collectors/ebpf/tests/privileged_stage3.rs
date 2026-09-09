//! Privileged eBPF workload smoke tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::time::{Duration, SystemTime};

use collector_binding::TraceBindingRequest;
use collector_event::RawObservationPayload;
use collector_instance::CollectorInstance;
use config_core::capture_profile::CaptureProfile;
use config_core::daemon::{EbpfCollectorConfig, EbpfEnabledMode, MemlockRlimit};
use config_core::trace_snapshot::CaptureProfileSnapshot;
use ebpf_collector::EbpfCollector;
use ebpf_collector::procfs::ProcfsIdentityReader;
use model_core::capability::{Capability, CapabilityRequest};
use model_core::ids::{ProfileName, TraceId};
use model_core::process::ProcessIdentity;
use process_identity::ProcessIdentityReader;

#[test]
#[ignore = "requires root/CAP_BPF, writable tracefs, kernel BTF, and eBPF attach support"]
fn every_compiled_program_passes_the_kernel_verifier() {
    let capabilities = vec![
        CapabilityRequest::required(Capability::ProcLifecycle),
        CapabilityRequest::required(Capability::ProcExecContext),
        CapabilityRequest::required(Capability::FsAccessBasic),
        CapabilityRequest::required(Capability::FsMmap),
        CapabilityRequest::required(Capability::NetTransport),
        CapabilityRequest::required(Capability::IpcPipeFifo),
        CapabilityRequest::required(Capability::IpcUnixSocket),
        CapabilityRequest::required(Capability::StdioChunk),
        CapabilityRequest::required(Capability::TlsPlaintextPayload),
    ];
    let profile = CaptureProfile::new(
        ProfileName::new("all-program-verifier-smoke"),
        capabilities.clone(),
    );
    let pid = std::process::id();
    let observation = ProcfsIdentityReader
        .read_identity(pid)
        .expect("read current process identity");
    let mut collector = EbpfCollector::new(EbpfCollectorConfig {
        enabled_mode: EbpfEnabledMode::True,
        enabled: true,
        memlock_rlimit: MemlockRlimit::Unlimited,
        tracked_process_max_entries: 4096,
        pending_operation_max_entries: 4096,
        event_ring_buffer_max_bytes: 1024 * 1024,
        tls_dynamic_loading: false,
        tls_scan_debounce: config_core::daemon::TlsScanDebounce::Image,
        tls_scan_interval_ms: 250,
    });

    collector
        .bind_trace(&TraceBindingRequest {
            trace_id: TraceId::new(1),
            root_identity: ProcessIdentity::new(1),
            root_observation: observation,
            root_namespace_pid: pid,
            profile_snapshot: CaptureProfileSnapshot::from_profile(&profile, SystemTime::now()),
            requested_capabilities: capabilities,
        })
        .unwrap_or_else(|error| panic!("load and attach every compiled BPF program: {error:?}"));
    collector
        .unbind_trace(TraceId::new(1))
        .expect("unbind verifier smoke trace");
}

#[test]
#[ignore = "requires root/CAP_BPF, writable tracefs, kernel BTF, and eBPF attach support"]
fn stage3_real_file_tcp_ipc_stdio_signal_workload() {
    let capabilities = vec![
        CapabilityRequest::required(Capability::ProcLifecycle),
        CapabilityRequest::required(Capability::ProcExecContext),
        CapabilityRequest::required(Capability::FsAccessBasic),
        CapabilityRequest::required(Capability::FsMmap),
        CapabilityRequest::required(Capability::NetTransport),
        CapabilityRequest::required(Capability::IpcPipeFifo),
        CapabilityRequest::required(Capability::IpcUnixSocket),
        CapabilityRequest::required(Capability::StdioChunk),
    ];
    let profile = CaptureProfile::new(ProfileName::new("stage3-smoke"), capabilities.clone());
    let pid = std::process::id();
    let observation = ProcfsIdentityReader
        .read_identity(pid)
        .expect("read current process identity");
    let mut collector = EbpfCollector::new(EbpfCollectorConfig {
        enabled_mode: EbpfEnabledMode::True,
        enabled: true,
        memlock_rlimit: MemlockRlimit::Unlimited,
        tracked_process_max_entries: 4096,
        pending_operation_max_entries: 4096,
        event_ring_buffer_max_bytes: 1024 * 1024,
        tls_dynamic_loading: false,
        tls_scan_debounce: config_core::daemon::TlsScanDebounce::Image,
        tls_scan_interval_ms: 250,
    });
    collector
        .bind_trace(&TraceBindingRequest {
            trace_id: TraceId::new(1),
            root_identity: ProcessIdentity::new(1),
            root_observation: observation,
            root_namespace_pid: pid,
            profile_snapshot: CaptureProfileSnapshot::from_profile(&profile, SystemTime::now()),
            requested_capabilities: capabilities,
        })
        .unwrap_or_else(|error| panic!("bind privileged eBPF collector: {error:?}"));

    run_file_workload();
    run_tcp_workload();
    run_pipe_workload();
    run_socketpair_workload();
    run_sendmsg_workload();
    let _ = std::process::Command::new("/bin/true")
        .arg("space separated")
        .arg(std::ffi::OsString::from_vec(vec![b'n', 0xff, b'x']))
        .status()
        .expect("exec argv workload");
    // SAFETY: the test owns the child PID and writes a valid static byte
    // buffer to standard error.
    unsafe {
        assert_eq!(libc::kill(pid as i32, libc::SIGCONT), 0);
        assert_eq!(
            libc::write(2, b"stage3-stdio-smoke\n".as_ptr().cast(), 19),
            19
        );
    }

    let mut observations = Vec::new();
    let mut payloads = Vec::new();
    for _ in 0..20 {
        let batch = collector.poll_batch().expect("poll eBPF events");
        observations.extend(batch.observations);
        payloads.extend(batch.payload_segments);
        if has_operation(&observations, "sendmsg")
            && has_operation(&observations, "socketpair")
            && has_operation(&observations, "signal")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(has_file_operation(&observations, "openat"));
    assert!(has_file_operation(&observations, "write"));
    assert!(has_file_operation(&observations, "read"));
    assert!(has_net_operation(&observations, "connect"));
    assert!(has_net_operation(&observations, "accept"));
    assert!(has_net_operation(&observations, "write"));
    assert!(has_net_operation(&observations, "read"));
    assert!(has_net_operation(&observations, "sendmsg"));
    assert!(has_ipc_operation(&observations, "pipe"));
    assert!(has_ipc_operation(&observations, "socketpair"));
    assert!(has_process_operation(&observations, "signal"));
    assert!(observations.iter().any(|event| {
        matches!(
            &event.payload,
            RawObservationPayload::Process { operation, argv: Some(argv), .. }
                if operation == "exec"
                    && argv.args.iter().any(|arg| arg == b"space separated")
                    && argv.args.iter().any(|arg| arg == &[b'n', 0xff, b'x'])
        )
    }));
    assert!(observations.iter().any(|event| {
        matches!(
            &event.payload,
            RawObservationPayload::Stdio { stream, .. } if stream == "stderr"
        )
    }));
    assert!(
        payloads.iter().any(|segment| {
            segment.bytes.as_deref() == Some(b"stage3-stdio-smoke\n".as_slice())
        })
    );
}

fn run_file_workload() {
    let path = format!("/tmp/censorscope-stage3-{}", std::process::id());
    let renamed = format!("{path}-renamed");
    let mut file = std::fs::File::create(&path).expect("create workload file");
    file.write_all(b"file-data").expect("write workload file");
    drop(file);
    let mut file = std::fs::File::open(&path).expect("reopen workload file");
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).expect("read workload file");
    assert_eq!(contents, b"file-data");
    drop(file);
    std::fs::rename(&path, &renamed).expect("rename workload file");
    std::fs::remove_file(&renamed).expect("remove workload file");
}

fn run_tcp_workload() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind TCP listener");
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).expect("connect TCP");
    let (mut server, _) = listener.accept().expect("accept TCP");
    client.write_all(b"ping").expect("send TCP");
    let mut bytes = [0_u8; 4];
    server.read_exact(&mut bytes).expect("receive TCP");
    assert_eq!(&bytes, b"ping");
}

fn run_pipe_workload() {
    let mut fds = [-1_i32; 2];
    // SAFETY: `fds` points to two writable integers and each descriptor is
    // closed exactly once after the synchronous pipe operations.
    unsafe {
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
        assert_eq!(libc::write(fds[1], b"p".as_ptr().cast(), 1), 1);
        let mut byte = 0_u8;
        assert_eq!(libc::read(fds[0], (&mut byte as *mut u8).cast(), 1), 1);
        assert_eq!(byte, b'p');
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

fn run_socketpair_workload() {
    let (mut first, mut second) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    first.write_all(b"u").expect("write socketpair");
    let mut byte = [0_u8; 1];
    second.read_exact(&mut byte).expect("read socketpair");
    assert_eq!(&byte, b"u");
}

fn run_sendmsg_workload() {
    let sender = UdpSocket::bind(("127.0.0.1", 0)).expect("bind UDP sender");
    let receiver = UdpSocket::bind(("127.0.0.1", 0)).expect("bind UDP receiver");
    sender
        .connect(receiver.local_addr().unwrap())
        .expect("connect UDP sender");
    receiver
        .connect(sender.local_addr().unwrap())
        .expect("connect UDP receiver");
    let bytes = b"message";
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: `msghdr` is a C POD used as an output/input buffer for sendmsg.
    let mut message = unsafe { std::mem::zeroed::<libc::msghdr>() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    // SAFETY: the socket and iovec are live for the duration of the call and
    // point to the immutable `bytes` payload.
    let sent = unsafe { libc::sendmsg(sender.as_raw_fd(), &message, 0) };
    assert_eq!(sent, bytes.len() as isize);
    let mut received = [0_u8; 16];
    assert_eq!(receiver.recv(&mut received).unwrap(), bytes.len());
}

fn has_operation(events: &[collector_event::RawCollectorEvent], operation: &str) -> bool {
    events.iter().any(|event| match &event.payload {
        RawObservationPayload::Process {
            operation: value, ..
        }
        | RawObservationPayload::File {
            operation: value, ..
        }
        | RawObservationPayload::Net {
            operation: value, ..
        }
        | RawObservationPayload::Ipc {
            operation: value, ..
        } => value == operation,
        _ => false,
    })
}

fn has_file_operation(events: &[collector_event::RawCollectorEvent], operation: &str) -> bool {
    events.iter().any(|event| {
        matches!(&event.payload, RawObservationPayload::File { operation: value, .. } if value == operation)
    })
}

fn has_net_operation(events: &[collector_event::RawCollectorEvent], operation: &str) -> bool {
    events.iter().any(|event| {
        matches!(&event.payload, RawObservationPayload::Net { operation: value, .. } if value == operation)
    })
}

fn has_ipc_operation(events: &[collector_event::RawCollectorEvent], operation: &str) -> bool {
    events.iter().any(|event| {
        matches!(&event.payload, RawObservationPayload::Ipc { operation: value, .. } if value == operation)
    })
}

fn has_process_operation(events: &[collector_event::RawCollectorEvent], operation: &str) -> bool {
    events.iter().any(|event| {
        matches!(&event.payload, RawObservationPayload::Process { operation: value, .. } if value == operation)
    })
}
