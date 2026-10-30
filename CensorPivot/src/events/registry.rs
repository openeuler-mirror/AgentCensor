use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use agentcensor_events::{CorrelationContext, ExternalEvent};

use crate::model::{BatchRequest, ToolCall};

#[derive(Clone, Debug)]
struct CallContext {
    call_id: String,
    started_at_ns: u64,
    ended_at_ns: Option<u64>,
}

#[derive(Clone, Debug)]
struct EventContext {
    transaction_id: String,
    trace_id: Option<u64>,
    session_id: String,
    runner_pid: u32,
    calls: Vec<CallContext>,
    finished_at_ns: Option<u64>,
}

const FINISHED_CONTEXT_RETENTION_NS: u64 = 5 * 60 * 1_000_000_000;

#[derive(Debug, Default)]
pub struct EventRegistry {
    contexts: Mutex<BTreeMap<String, EventContext>>,
}

impl EventRegistry {
    pub fn register_runner(&self, request: &BatchRequest, runner_pid: u32) {
        let transaction_id = request
            .guard_scope
            .strip_prefix("pivot-")
            .unwrap_or(&request.guard_scope)
            .to_string();
        if let Ok(mut contexts) = self.contexts.lock() {
            prune_finished(&mut contexts, now_ns());
            contexts.insert(
                request.guard_scope.clone(),
                EventContext {
                    transaction_id,
                    trace_id: request.trace_id,
                    session_id: request.session_id.clone(),
                    runner_pid,
                    calls: Vec::new(),
                    finished_at_ns: None,
                },
            );
        }
    }

    pub fn set_trace(&self, guard_scope: &str, trace_id: u64) {
        if let Ok(mut contexts) = self.contexts.lock()
            && let Some(context) = contexts.get_mut(guard_scope)
        {
            context.trace_id = Some(trace_id);
        }
    }

    pub fn call_start(&self, guard_scope: &str, call: &ToolCall) {
        if let Ok(mut contexts) = self.contexts.lock()
            && let Some(context) = contexts.get_mut(guard_scope)
        {
            context.calls.push(CallContext {
                call_id: call.id.clone(),
                started_at_ns: now_ns(),
                ended_at_ns: None,
            });
        }
    }

    pub fn call_end(&self, guard_scope: &str, call_id: &str) {
        if let Ok(mut contexts) = self.contexts.lock()
            && let Some(context) = contexts.get_mut(guard_scope)
            && let Some(call) = context
                .calls
                .iter_mut()
                .rev()
                .find(|call| call.call_id == call_id && call.ended_at_ns.is_none())
        {
            call.ended_at_ns = Some(now_ns());
        }
    }

    pub fn correlate(&self, event: &ExternalEvent) -> Option<CorrelationContext> {
        let domain = event.subject.domain.as_deref()?;
        let mut contexts = self.contexts.lock().ok()?;
        prune_finished(&mut contexts, now_ns());
        let context = contexts.get(domain)?;
        let call_id = context
            .calls
            .iter()
            .rev()
            .find(|call| {
                event.occurred_at_ns >= call.started_at_ns
                    && call
                        .ended_at_ns
                        .is_none_or(|ended| event.occurred_at_ns <= ended)
            })
            .map(|call| call.call_id.clone());
        Some(CorrelationContext {
            transaction_id: Some(context.transaction_id.clone()),
            trace_id: context.trace_id,
            session_id: Some(context.session_id.clone()),
            call_id,
        })
    }

    pub fn finish_batch(&self, guard_scope: &str) {
        if let Ok(mut contexts) = self.contexts.lock()
            && let Some(context) = contexts.get_mut(guard_scope)
        {
            context.finished_at_ns = Some(now_ns());
        }
    }

    pub fn runner_pid(&self, guard_scope: &str) -> Option<u32> {
        self.contexts
            .lock()
            .ok()?
            .get(guard_scope)
            .map(|context| context.runner_pid)
    }
}

fn prune_finished(contexts: &mut BTreeMap<String, EventContext>, now: u64) {
    contexts.retain(|_, context| {
        context
            .finished_at_ns
            .is_none_or(|finished| now.saturating_sub(finished) <= FINISHED_CONTEXT_RETENTION_NS)
    });
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}
