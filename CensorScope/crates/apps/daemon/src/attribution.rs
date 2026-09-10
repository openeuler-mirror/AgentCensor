//! Unique-window call attribution shared by ingest and queued backfill jobs.
//!
//! Ingest attributes an event the moment it is drained using the live
//! in-memory span registry; a queued job re-runs the *same* decision rule
//! against the committed spans (as of job execution) so both paths can never
//! drift apart.

use std::time::SystemTime;

use model_core::ids::TraceId;
use model_core::process::SessionIdentity;
use storage_core::CallSpanRecord;

/// The single decision rule for span-window call attribution. Returns the call
/// id only when the observed event falls inside exactly one matching span.
///
/// A child shell differs in pid from the span host that spawned it, so the pid
/// is only a disambiguator, never a hard requirement. Events without a session
/// id match any span of the trace (mirroring ingest behaviour).
pub fn unique_call_window_match(
    spans: &[CallSpanRecord],
    trace_id: TraceId,
    host_pid: Option<u32>,
    session_id: &Option<SessionIdentity>,
    observed_at: SystemTime,
) -> Option<String> {
    let session_matches = |span: &CallSpanRecord| {
        session_id.as_ref().is_none_or(|session| {
            span.session_id.as_deref() == Some(session.as_str())
        })
    };
    let in_window = |span: &CallSpanRecord| {
        observed_at >= span.started_at
            && span.ended_at.is_none_or(|end| observed_at <= end)
    };
    let mut candidates = spans
        .iter()
        .filter(|span| {
            span.trace_id == trace_id && session_matches(span) && in_window(span)
        })
        .map(|span| span.call_id.as_str())
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        // The host pid is only a tie-breaker: prefer it when it leaves one
        // unambiguous candidate, keeping the session/time-only path for
        // descendant processes.
        let pid_candidates = spans
            .iter()
            .filter(|span| {
                span.trace_id == trace_id
                    && Some(span.host_pid) == host_pid
                    && session_matches(span)
                    && in_window(span)
            })
            .map(|span| span.call_id.as_str())
            .collect::<Vec<_>>();
        if pid_candidates.len() == 1 {
            candidates = pid_candidates;
        }
    }
    (candidates.len() == 1).then(|| candidates[0].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_core::process::SessionIdentity;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    fn span(
        trace: u64,
        session: Option<&str>,
        call: &str,
        pid: u32,
        start: u64,
        end: Option<u64>,
    ) -> CallSpanRecord {
        CallSpanRecord {
            trace_id: TraceId::new(trace),
            session_id: session.map(str::to_string),
            call_id: call.to_string(),
            host_pid: pid,
            started_at: at(start),
            ended_at: end.map(at),
            status: None,
        }
    }

    #[test]
    fn unique_window_match_prefers_pid_only_as_tie_breaker() {
        let session = Some(SessionIdentity::new("s1"));
        let spans = vec![
            span(1, Some("s1"), "call-a", 100, 10, Some(20)),
            span(1, Some("s1"), "call-b", 200, 15, Some(30)),
        ];
        // Inside both windows but the host pid narrows to exactly one.
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), Some(100), &session, at(16)),
            Some("call-a".to_string())
        );
        // The sibling's host pid resolves to the sibling.
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), Some(200), &session, at(16)),
            Some("call-b".to_string())
        );
        // A process with neither pid (e.g. a helper) is ambiguous -> None.
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), Some(999), &session, at(16)),
            None
        );
        // A descendant with an unrelated pid still resolves by window alone.
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), Some(999), &session, at(11)),
            Some("call-a".to_string())
        );
        // Outside every window -> None.
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), Some(100), &session, at(31)),
            None
        );
    }

    #[test]
    fn sessionless_events_match_any_span_of_the_trace() {
        let spans = vec![
            span(1, Some("s1"), "call-a", 100, 10, Some(20)),
            span(1, Some("s2"), "call-b", 200, 10, Some(20)),
        ];
        let none = None;
        // No session on the event: both spans cover it -> ambiguous.
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), None, &none, at(12)),
            None
        );
        // But once a session is known the other session's span is excluded.
        let session = Some(SessionIdentity::new("s2"));
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), None, &session, at(12)),
            Some("call-b".to_string())
        );
        // Other trace ids never match.
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(9), None, &none, at(12)),
            None
        );
    }

    #[test]
    fn open_spans_cover_until_closed_and_closed_spans_exclude_later_events() {
        let session = Some(SessionIdentity::new("s1"));
        let spans = vec![
            span(1, Some("s1"), "call-open", 100, 10, None),
            span(1, Some("s1"), "call-closed", 200, 40, Some(50)),
        ];
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), Some(100), &session, at(500)),
            Some("call-open".to_string())
        );
        assert_eq!(
            unique_call_window_match(&spans, TraceId::new(1), Some(200), &session, at(45)),
            Some("call-closed".to_string())
        );
    }
}
