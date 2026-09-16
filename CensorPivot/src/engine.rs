use crate::error::{PivotError, Result};
use crate::model::*;
use crate::store::TransactionStore;
use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub trait CensorFs: Send + Sync {
    fn open(&self, record: &TransactionRecord) -> Result<FsResources>;
    fn prepare(&self, record: &TransactionRecord) -> Result<String>;
    fn publish(&self, record: &TransactionRecord) -> Result<()>;
    fn abort(&self, record: &TransactionRecord) -> Result<()>;
}

pub trait CensorScope: Send + Sync {
    fn required(&self) -> bool;
    fn start_batch(&self, request: &BatchRequest, host_pid: u32) -> Result<Option<u64>>;
    fn call_start(
        &self,
        request: &BatchRequest,
        call: &ToolCall,
        host_pid: u32,
        trace_id: Option<u64>,
    ) -> Result<()>;
    fn call_end(
        &self,
        request: &BatchRequest,
        call: &ToolCall,
        host_pid: u32,
        trace_id: Option<u64>,
        status: &str,
    ) -> Result<()>;
    fn finish_batch(
        &self,
        request: &BatchRequest,
        host_pid: u32,
        trace_id: Option<u64>,
    ) -> Result<()>;
}

pub trait ToolSession: Send {
    fn host_pid(&self) -> u32;
    fn run(&mut self, call: &ToolCall) -> Result<CallResult>;
    fn finish(&mut self) -> Result<()>;
}

pub trait ToolRunner: Send + Sync {
    fn start(&self, request: &BatchRequest, fs: &FsResources) -> Result<Box<dyn ToolSession>>;
}

pub struct Engine<F, S, R> {
    store: TransactionStore,
    fs: F,
    scope: S,
    runner: R,
    max_calls: usize,
}

impl<F, S, R> Engine<F, S, R>
where
    F: CensorFs,
    S: CensorScope,
    R: ToolRunner,
{
    pub const fn new(
        store: TransactionStore,
        fs: F,
        scope: S,
        runner: R,
        max_calls: usize,
    ) -> Self {
        Self {
            store,
            fs,
            scope,
            runner,
            max_calls,
        }
    }

    pub fn execute(&self, mut request: BatchRequest, actor_uid: u32) -> Result<TransactionRecord> {
        validate_request(&request, self.max_calls)?;
        let fingerprint = fingerprint(&request)?;
        if let Some(existing) = self.store.find_request(&request.request_id)? {
            authorize(&existing, actor_uid)?;
            if existing.request_fingerprint != fingerprint {
                return Err(PivotError::Conflict(
                    "request_id was already used with a different batch".into(),
                ));
            }
            return Ok(existing);
        }

        // Keep the caller's original request fingerprint; derive the actual domain from
        // the durable transaction ID so no caller can rebind another batch's domain.
        let transaction_id = Uuid::new_v4();
        request.guard_scope = format!("pivot-{transaction_id}");
        let now = now_ms();
        let mut record = TransactionRecord {
            schema_version: SCHEMA_VERSION,
            transaction_id,
            request_id: request.request_id.clone(),
            request_fingerprint: fingerprint,
            actor_uid,
            state: TransactionState::Received,
            decision: None,
            summary: RequestSummary::from(&request),
            fs: FsResources::default(),
            open_request_id: Uuid::new_v4(),
            prepare_request_id: Uuid::new_v4(),
            finish_request_id: Uuid::new_v4(),
            results: Vec::new(),
            warnings: Vec::new(),
            last_error: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.store.save(&record)?;
        record.state = TransactionState::Preparing;
        self.persist(&mut record)?;

        record.fs.open_attempted = true;
        self.persist(&mut record)?;
        match self.fs.open(&record) {
            Ok(resources) => {
                record.fs = resources;
                self.persist(&mut record)?;
            }
            Err(error) => return self.abort_after_error(record, error),
        }

        let mut session = match self.runner.start(&request, &record.fs) {
            Ok(session) => session,
            Err(error) => return self.abort_after_error(record, error),
        };
        let runner_pid = session.host_pid();
        let trace_id = match self.scope.start_batch(&request, runner_pid) {
            Ok(trace_id) => trace_id,
            Err(error) if self.scope.required() => {
                let _ = session.finish();
                return self.abort_after_error(record, error);
            }
            Err(error) => {
                record.warnings.push(error.to_string());
                self.persist(&mut record)?;
                None
            }
        };
        record.summary.trace_id = trace_id;
        self.persist(&mut record)?;
        for call in &request.calls {
            match self.scope.call_start(&request, call, runner_pid, trace_id) {
                Err(error) if self.scope.required() => {
                    let _ = session.finish();
                    let _ = self.finish_scope(&request, runner_pid, trace_id, &mut record);
                    return self.abort_after_error(record, error);
                }
                Err(error) => {
                    record.warnings.push(error.to_string());
                    self.persist(&mut record)?;
                }
                Ok(()) => {}
            }
            let result = match session.run(call) {
                Ok(result) => result,
                Err(error) => {
                    let _ = self
                        .scope
                        .call_end(&request, call, runner_pid, trace_id, "error");
                    let _ = session.finish();
                    let _ = self.finish_scope(&request, runner_pid, trace_id, &mut record);
                    return self.abort_after_error(record, error);
                }
            };
            let status = scope_status(&result);
            let succeeded = result.succeeded();
            record.results.push(result);
            self.persist(&mut record)?;
            match self
                .scope
                .call_end(&request, call, runner_pid, trace_id, status)
            {
                Err(error) if self.scope.required() => {
                    let _ = session.finish();
                    let _ = self.finish_scope(&request, runner_pid, trace_id, &mut record);
                    return self.abort_after_error(record, error);
                }
                Err(error) => {
                    record.warnings.push(error.to_string());
                    self.persist(&mut record)?;
                }
                Ok(()) => {}
            }
            if !succeeded {
                let _ = session.finish();
                let _ = self.finish_scope(&request, runner_pid, trace_id, &mut record);
                return self.abort_after_error(
                    record,
                    PivotError::component("runner", format!("tool call {} failed", call.id)),
                );
            }
        }
        if let Err(error) = session.finish() {
            let _ = self.finish_scope(&request, runner_pid, trace_id, &mut record);
            return self.abort_after_error(record, error);
        }
        if let Err(error) = self.finish_scope(&request, runner_pid, trace_id, &mut record) {
            return self.abort_after_error(record, error);
        }

        match self.fs.prepare(&record) {
            Ok(candidate_id) => record.fs.candidate_id = Some(candidate_id),
            Err(error) => return self.abort_after_error(record, error),
        }
        record.state = TransactionState::Prepared;
        self.persist(&mut record)?;
        self.decide(&mut record, Decision::Commit)?;
        self.complete(&mut record)?;
        Ok(record)
    }

    pub fn status(&self, transaction_id: Uuid, actor_uid: u32) -> Result<TransactionRecord> {
        let record = self.store.load(transaction_id)?;
        authorize(&record, actor_uid)?;
        Ok(record)
    }

    pub fn recover(&self, actor_uid: Option<u32>) -> Result<Vec<TransactionRecord>> {
        let mut recovered = Vec::new();
        for mut record in self.store.list()? {
            if actor_uid.is_some_and(|uid| uid != 0 && uid != record.actor_uid) {
                continue;
            }
            if record.state.terminal() {
                continue;
            }
            match record.state {
                TransactionState::Received => {
                    self.decide(&mut record, Decision::Abort)?;
                }
                TransactionState::Preparing => {
                    self.decide(&mut record, Decision::Abort)?;
                }
                TransactionState::Prepared => self.decide(&mut record, Decision::Commit)?,
                TransactionState::CommitDecided
                | TransactionState::AbortDecided
                | TransactionState::Committed
                | TransactionState::Aborted => {}
            }
            self.complete(&mut record)?;
            if actor_uid.is_none_or(|uid| uid == 0 || uid == record.actor_uid) {
                recovered.push(record);
            }
        }
        Ok(recovered)
    }

    fn abort_after_error(
        &self,
        mut record: TransactionRecord,
        error: PivotError,
    ) -> Result<TransactionRecord> {
        record.last_error = Some(error.to_string());
        self.decide(&mut record, Decision::Abort)?;
        self.complete(&mut record)?;
        Ok(record)
    }

    fn decide(&self, record: &mut TransactionRecord, decision: Decision) -> Result<()> {
        if let Some(existing) = record.decision {
            if existing != decision {
                return Err(PivotError::Conflict(format!(
                    "transaction is already decided {existing:?}"
                )));
            }
            return Ok(());
        }
        record.decision = Some(decision);
        record.state = match decision {
            Decision::Commit => TransactionState::CommitDecided,
            Decision::Abort => TransactionState::AbortDecided,
        };
        self.persist(record)
    }

    fn complete(&self, record: &mut TransactionRecord) -> Result<()> {
        match record.decision {
            Some(Decision::Commit) => match self.fs.publish(record) {
                Ok(()) => {
                    record.state = TransactionState::Committed;
                    record.last_error = None;
                }
                Err(error) => {
                    record.state = TransactionState::CommitDecided;
                    record.last_error = Some(error.to_string());
                }
            },
            Some(Decision::Abort) => {
                if record.fs.open_attempted && record.fs.ticket_id.is_none() {
                    match self.fs.open(record) {
                        Ok(resources) => {
                            record.fs = resources;
                            self.persist(record)?;
                        }
                        Err(error) => {
                            record.state = TransactionState::AbortDecided;
                            record.last_error = Some(error.to_string());
                            return self.persist(record);
                        }
                    }
                }
                match self.fs.abort(record) {
                    Ok(()) => {
                        record.state = TransactionState::Aborted;
                    }
                    Err(error) => {
                        record.state = TransactionState::AbortDecided;
                        record.last_error = Some(error.to_string());
                    }
                }
            }
            None => {
                return Err(PivotError::Protocol(
                    "cannot complete an undecided transaction".into(),
                ));
            }
        }
        self.persist(record)
    }

    fn persist(&self, record: &mut TransactionRecord) -> Result<()> {
        record.updated_at_ms = now_ms();
        self.store.save(record)
    }

    fn finish_scope(
        &self,
        request: &BatchRequest,
        host_pid: u32,
        trace_id: Option<u64>,
        record: &mut TransactionRecord,
    ) -> Result<()> {
        match self.scope.finish_batch(request, host_pid, trace_id) {
            Ok(()) => Ok(()),
            Err(error) if self.scope.required() => Err(error),
            Err(error) => {
                record.warnings.push(error.to_string());
                self.persist(record)
            }
        }
    }
}

fn scope_status(result: &CallResult) -> &'static str {
    if result.timed_out {
        "timeout"
    } else if result.exit_code == 0 {
        "success"
    } else {
        "error"
    }
}

fn authorize(record: &TransactionRecord, actor_uid: u32) -> Result<()> {
    if actor_uid == 0 || record.actor_uid == actor_uid {
        Ok(())
    } else {
        Err(PivotError::Conflict(
            "transaction belongs to another uid".into(),
        ))
    }
}

fn fingerprint(request: &BatchRequest) -> Result<String> {
    let encoded = serde_json::to_vec(request)?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn validate_request(request: &BatchRequest, max_calls: usize) -> Result<()> {
    validate_token("request_id", &request.request_id)?;
    validate_token("session_id", &request.session_id)?;
    validate_token("branch", &request.branch)?;
    validate_token("expected_generation", &request.expected_generation)?;
    validate_token("guard_scope", &request.guard_scope)?;
    if request.calls.is_empty() || request.calls.len() > max_calls {
        return Err(PivotError::Invalid(format!(
            "calls must contain between 1 and {max_calls} entries"
        )));
    }
    if request.trace_id == Some(0) {
        return Err(PivotError::Invalid(
            "trace_id must be greater than zero".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    for call in &request.calls {
        validate_token("call.id", &call.id)?;
        if !ids.insert(&call.id) {
            return Err(PivotError::Invalid(format!(
                "duplicate call id {}",
                call.id
            )));
        }
        if !Path::new(&call.program).is_absolute() {
            return Err(PivotError::Invalid(format!(
                "tool program must be absolute: {}",
                call.program
            )));
        }
        if call.timeout_ms == 0 || call.timeout_ms > 86_400_000 {
            return Err(PivotError::Invalid(format!(
                "call {} timeout must be between 1 and 86400000 ms",
                call.id
            )));
        }
        let cwd = Path::new(&call.cwd);
        if cwd.is_absolute()
            || cwd.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(PivotError::Invalid(format!(
                "call {} cwd must stay below /workspace",
                call.id
            )));
        }
        for key in call.env.keys() {
            if key.starts_with("CENSORGUARD_")
                || key.starts_with("CENSORSCOPE_")
                || key.starts_with("CENSORPIVOT_")
                || matches!(key.as_str(), "DSH_CENSORSCOPE_CALL_ID" | "DSH_SESSION_ID")
            {
                return Err(PivotError::Invalid(format!(
                    "call {} may not override reserved environment key {key}",
                    call.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_token(name: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(PivotError::Invalid(format!(
            "{name} is empty, too long, or contains control characters"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeFs {
        actions: Arc<Mutex<Vec<&'static str>>>,
        fail_open_times: Arc<Mutex<usize>>,
        fail_publish_once: Arc<Mutex<bool>>,
    }

    impl CensorFs for FakeFs {
        fn open(&self, _: &TransactionRecord) -> Result<FsResources> {
            self.actions.lock().map_err(lock_error)?.push("open");
            let mut failures = self.fail_open_times.lock().map_err(lock_error)?;
            if *failures > 0 {
                *failures -= 1;
                return Err(PivotError::component("fake-fs", "open reply lost"));
            }
            Ok(FsResources {
                open_attempted: true,
                ticket_id: Some("ticket".into()),
                view_id: Some("view".into()),
                candidate_id: None,
                owner_uid: Some(1000),
                owner_gid: Some(1000),
            })
        }
        fn prepare(&self, _: &TransactionRecord) -> Result<String> {
            self.actions.lock().map_err(lock_error)?.push("prepare");
            Ok("candidate".into())
        }
        fn publish(&self, _: &TransactionRecord) -> Result<()> {
            self.actions.lock().map_err(lock_error)?.push("publish");
            let mut fail = self.fail_publish_once.lock().map_err(lock_error)?;
            if *fail {
                *fail = false;
                Err(PivotError::component("fake-fs", "publish failed"))
            } else {
                Ok(())
            }
        }
        fn abort(&self, _: &TransactionRecord) -> Result<()> {
            self.actions.lock().map_err(lock_error)?.push("abort");
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct NoScope;
    impl CensorScope for NoScope {
        fn required(&self) -> bool {
            false
        }
        fn start_batch(&self, request: &BatchRequest, _: u32) -> Result<Option<u64>> {
            Ok(request.trace_id)
        }
        fn call_start(&self, _: &BatchRequest, _: &ToolCall, _: u32, _: Option<u64>) -> Result<()> {
            Ok(())
        }
        fn call_end(
            &self,
            _: &BatchRequest,
            _: &ToolCall,
            _: u32,
            _: Option<u64>,
            _: &str,
        ) -> Result<()> {
            Ok(())
        }
        fn finish_batch(&self, _: &BatchRequest, _: u32, _: Option<u64>) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingScope {
        actions: Arc<Mutex<Vec<String>>>,
    }

    impl CensorScope for RecordingScope {
        fn required(&self) -> bool {
            true
        }

        fn start_batch(&self, _: &BatchRequest, host_pid: u32) -> Result<Option<u64>> {
            self.actions
                .lock()
                .map_err(lock_error)?
                .push(format!("start:{host_pid}"));
            Ok(Some(99))
        }

        fn call_start(
            &self,
            _: &BatchRequest,
            call: &ToolCall,
            _: u32,
            trace_id: Option<u64>,
        ) -> Result<()> {
            self.actions
                .lock()
                .map_err(lock_error)?
                .push(format!("call-start:{}:{trace_id:?}", call.id));
            Ok(())
        }

        fn call_end(
            &self,
            _: &BatchRequest,
            call: &ToolCall,
            _: u32,
            trace_id: Option<u64>,
            status: &str,
        ) -> Result<()> {
            self.actions
                .lock()
                .map_err(lock_error)?
                .push(format!("call-end:{}:{trace_id:?}:{status}", call.id));
            Ok(())
        }

        fn finish_batch(&self, _: &BatchRequest, _: u32, trace_id: Option<u64>) -> Result<()> {
            self.actions
                .lock()
                .map_err(lock_error)?
                .push(format!("finish:{trace_id:?}"));
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeRunner(i32);
    struct FakeSession(i32);

    impl ToolSession for FakeSession {
        fn host_pid(&self) -> u32 {
            42
        }

        fn run(&mut self, call: &ToolCall) -> Result<CallResult> {
            Ok(CallResult {
                call_id: call.id.clone(),
                exit_code: self.0,
                timed_out: false,
                duration_ms: 1,
                stdout: String::new(),
                stderr: String::new(),
                output_truncated: false,
            })
        }

        fn finish(&mut self) -> Result<()> {
            Ok(())
        }
    }

    impl ToolRunner for FakeRunner {
        fn start(&self, _: &BatchRequest, _: &FsResources) -> Result<Box<dyn ToolSession>> {
            Ok(Box::new(FakeSession(self.0)))
        }
    }

    fn lock_error<T>(_: std::sync::PoisonError<T>) -> PivotError {
        PivotError::Protocol("test lock poisoned".into())
    }

    fn batch() -> BatchRequest {
        BatchRequest {
            request_id: "request-1".into(),
            session_id: "session-1".into(),
            trace_id: Some(1),
            branch: "main".into(),
            expected_generation: "gen-1".into(),
            expected_head_seq: 1,
            guard_scope: "scope-1".into(),
            guard_group: "strict".into(),
            calls: vec![ToolCall {
                id: "call-1".into(),
                program: "/bin/true".into(),
                args: Vec::new(),
                cwd: String::new(),
                env: Default::default(),
                timeout_ms: 1000,
            }],
        }
    }

    #[test]
    fn successful_batch_prepares_decides_then_publishes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let fs = FakeFs::default();
        let actions = Arc::clone(&fs.actions);
        let engine = Engine::new(
            TransactionStore::open(temp.path())?,
            fs,
            NoScope,
            FakeRunner(0),
            8,
        );
        let record = engine.execute(batch(), 1000)?;
        assert_eq!(record.state, TransactionState::Committed);
        assert_eq!(record.decision, Some(Decision::Commit));
        assert_eq!(
            record.summary.guard_scope,
            format!("pivot-{}", record.transaction_id)
        );
        assert_eq!(
            *actions.lock().map_err(lock_error)?,
            vec!["open", "prepare", "publish"]
        );
        Ok(())
    }

    #[test]
    fn scope_tracks_the_ready_runner_for_the_whole_batch() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let scope = RecordingScope::default();
        let actions = Arc::clone(&scope.actions);
        let mut request = batch();
        request.trace_id = None;
        let engine = Engine::new(
            TransactionStore::open(temp.path())?,
            FakeFs::default(),
            scope,
            FakeRunner(0),
            8,
        );
        let record = engine.execute(request, 1000)?;
        assert_eq!(record.summary.trace_id, Some(99));
        assert_eq!(
            *actions.lock().map_err(lock_error)?,
            vec![
                "start:42",
                "call-start:call-1:Some(99)",
                "call-end:call-1:Some(99):success",
                "finish:Some(99)",
            ]
        );
        Ok(())
    }

    #[test]
    fn failed_tool_durably_aborts_without_prepare() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let fs = FakeFs::default();
        let actions = Arc::clone(&fs.actions);
        let engine = Engine::new(
            TransactionStore::open(temp.path())?,
            fs,
            NoScope,
            FakeRunner(7),
            8,
        );
        let record = engine.execute(batch(), 1000)?;
        assert_eq!(record.state, TransactionState::Aborted);
        assert_eq!(record.decision, Some(Decision::Abort));
        assert_eq!(*actions.lock().map_err(lock_error)?, vec!["open", "abort"]);
        Ok(())
    }

    #[test]
    fn recovery_retries_commit_and_never_aborts() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let fs = FakeFs::default();
        *fs.fail_publish_once.lock().map_err(lock_error)? = true;
        let actions = Arc::clone(&fs.actions);
        let engine = Engine::new(
            TransactionStore::open(temp.path())?,
            fs,
            NoScope,
            FakeRunner(0),
            8,
        );
        let first = engine.execute(batch(), 1000)?;
        assert_eq!(first.state, TransactionState::CommitDecided);
        let records = engine.recover(None)?;
        assert_eq!(records[0].state, TransactionState::Committed);
        let actions = actions.lock().map_err(lock_error)?;
        assert_eq!(
            actions.iter().filter(|action| **action == "abort").count(),
            0
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| **action == "publish")
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn recovery_does_not_mutate_another_users_pending_transaction() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let fs = FakeFs::default();
        *fs.fail_publish_once.lock().map_err(lock_error)? = true;
        let actions = Arc::clone(&fs.actions);
        let engine = Engine::new(
            TransactionStore::open(temp.path())?,
            fs,
            NoScope,
            FakeRunner(0),
            8,
        );
        let record = engine.execute(batch(), 1000)?;
        assert_eq!(record.state, TransactionState::CommitDecided);
        let before = actions.lock().map_err(lock_error)?.len();
        assert!(engine.recover(Some(2000))?.is_empty());
        assert_eq!(before, actions.lock().map_err(lock_error)?.len());
        assert_eq!(
            engine.status(record.transaction_id, 1000)?.state,
            TransactionState::CommitDecided
        );
        assert_eq!(
            engine.recover(Some(1000))?[0].state,
            TransactionState::Committed
        );
        Ok(())
    }

    #[test]
    fn duplicate_request_is_idempotent() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let fs = FakeFs::default();
        let actions = Arc::clone(&fs.actions);
        let engine = Engine::new(
            TransactionStore::open(temp.path())?,
            fs,
            NoScope,
            FakeRunner(0),
            8,
        );
        let first = engine.execute(batch(), 1000)?;
        let second = engine.execute(batch(), 1000)?;
        assert_eq!(first.transaction_id, second.transaction_id);
        assert_eq!(actions.lock().map_err(lock_error)?.len(), 3);
        Ok(())
    }

    #[test]
    fn uncertain_open_stays_abort_decided_until_resource_is_recovered() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let fs = FakeFs::default();
        *fs.fail_open_times.lock().map_err(lock_error)? = 2;
        let actions = Arc::clone(&fs.actions);
        let engine = Engine::new(
            TransactionStore::open(temp.path())?,
            fs,
            NoScope,
            FakeRunner(0),
            8,
        );
        let first = engine.execute(batch(), 1000)?;
        assert_eq!(first.state, TransactionState::AbortDecided);
        let records = engine.recover(None)?;
        assert_eq!(records[0].state, TransactionState::Aborted);
        assert_eq!(
            *actions.lock().map_err(lock_error)?,
            vec!["open", "open", "open", "abort"]
        );
        Ok(())
    }

    #[test]
    fn maps_runner_results_to_censorscope_status_vocabulary() {
        let result = |exit_code, timed_out| CallResult {
            call_id: "call".into(),
            exit_code,
            timed_out,
            duration_ms: 1,
            stdout: String::new(),
            stderr: String::new(),
            output_truncated: false,
        };
        assert_eq!(scope_status(&result(0, false)), "success");
        assert_eq!(scope_status(&result(7, false)), "error");
        assert_eq!(scope_status(&result(128, true)), "timeout");
    }
}
