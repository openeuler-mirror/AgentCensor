//! Conversion from collector observations to storage-ready domain events.

use model_core::event::{
    ApplicationPayload, DomainEvent, EventEnvelope, EventFlags, EventPayload, FilePayload,
    IpcPayload, LossPayload, NetPayload, ProcessPayload, StdioPayload,
};
use model_core::ids::{EventId, TraceId};
use model_core::process::ProcessIdentity;

/// Normalizes one collector observation after runtime identity resolution.
pub fn normalize_event(
    raw_event: collector_event::RawCollectorEvent,
    trace_id: TraceId,
    process: ProcessIdentity,
    parent: Option<ProcessIdentity>,
    event_id: EventId,
) -> DomainEvent {
    let mut flags = EventFlags::empty();
    let identity_gap =
        raw_event.envelope.process.host.as_ref().is_none_or(|host| {
            host.start_time_ticks == 0 && host.start_boottime_ns.unwrap_or(0) == 0
        });
    if identity_gap {
        flags = flags
            .union(EventFlags::PARTIAL)
            .union(EventFlags::CAPTURE_GAP);
    }
    if let collector_event::RawObservationPayload::File { metadata, .. } = &raw_event.payload {
        if metadata.get("path_truncated").map(String::as_str) == Some("true") {
            flags = flags
                .union(EventFlags::PARTIAL)
                .union(EventFlags::TRUNCATED);
        }
        if metadata.get("path_capture_gap").map(String::as_str) == Some("true") {
            flags = flags
                .union(EventFlags::PARTIAL)
                .union(EventFlags::CAPTURE_GAP);
        }
    }
    if let collector_event::RawObservationPayload::Net { metadata, .. } = &raw_event.payload {
        if metadata.get("endpoint_capture_gap").map(String::as_str) == Some("true") {
            flags = flags
                .union(EventFlags::PARTIAL)
                .union(EventFlags::CAPTURE_GAP);
        }
    }
    if let collector_event::RawObservationPayload::Stdio { metadata, .. } = &raw_event.payload {
        if metadata.get("capture_gap").map(String::as_str) == Some("true") {
            flags = flags
                .union(EventFlags::PARTIAL)
                .union(EventFlags::CAPTURE_GAP);
        }
    }
    if let collector_event::RawObservationPayload::Process {
        argv: Some(argv), ..
    } = &raw_event.payload
    {
        if argv.flags & model_core::process::ArgvCapture::PARTIAL != 0 {
            flags = flags.union(EventFlags::PARTIAL);
        }
        if argv.flags & model_core::process::ArgvCapture::TRUNCATED != 0 {
            flags = flags
                .union(EventFlags::PARTIAL)
                .union(EventFlags::TRUNCATED);
        }
        if argv.flags & model_core::process::ArgvCapture::READ_FAILURE != 0 {
            flags = flags
                .union(EventFlags::PARTIAL)
                .union(EventFlags::CAPTURE_GAP);
        }
        if argv.flags & model_core::process::ArgvCapture::LOSS != 0 {
            flags = flags.union(EventFlags::PARTIAL).union(EventFlags::LOSS);
        }
    }
    if matches!(
        raw_event.payload,
        collector_event::RawObservationPayload::Loss { .. }
    ) {
        flags = flags.union(EventFlags::LOSS);
    }
    let envelope = EventEnvelope {
        event_id,
        trace_id,
        observed_at: raw_event.envelope.observed_at,
        process,
        collector: raw_event.envelope.collector,
        kind: model_core::event::EventKind::Unknown,
        flags,
        session_id: None,
        call_id: raw_event.envelope.call_id.clone(),
    };
    let payload = match raw_event.payload {
        collector_event::RawObservationPayload::Process {
            operation,
            argv,
            metadata,
            ..
        } => {
            let executable = metadata
                .get("executable")
                .cloned()
                .filter(|value| !value.is_empty());
            EventPayload::Process(ProcessPayload {
                operation,
                parent,
                executable,
                argv,
                metadata,
            })
        }
        collector_event::RawObservationPayload::File {
            operation,
            path,
            fd,
            size,
            metadata,
        } => EventPayload::File(FilePayload {
            operation,
            path,
            fd,
            size,
            metadata,
        }),
        collector_event::RawObservationPayload::Net {
            operation,
            endpoint,
            direction,
            length,
            result,
            metadata,
        } => EventPayload::Net(NetPayload {
            operation,
            endpoint,
            direction,
            length,
            result,
            metadata,
        }),
        collector_event::RawObservationPayload::Ipc {
            operation,
            channel,
            direction,
            length,
            metadata,
        } => EventPayload::Ipc(IpcPayload {
            operation,
            channel,
            direction,
            length,
            metadata,
        }),
        collector_event::RawObservationPayload::Stdio {
            stream,
            direction,
            length,
            metadata,
        } => EventPayload::Stdio(StdioPayload {
            stream,
            direction,
            length,
            metadata,
        }),
        collector_event::RawObservationPayload::Application { protocol, metadata } => {
            EventPayload::Application(ApplicationPayload { protocol, metadata })
        }
        collector_event::RawObservationPayload::Loss {
            reason,
            dropped,
            bytes,
        } => EventPayload::Loss(LossPayload {
            reason,
            dropped,
            bytes,
        }),
    };
    DomainEvent::new(envelope, payload)
}
