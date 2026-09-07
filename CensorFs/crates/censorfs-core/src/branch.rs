use crate::codec::{self, RecordKind};
use crate::error::{ErrorCode, Result, CensorFsError};
use crate::fault::FaultInjector;
use crate::ids::*;
use crate::model::*;
use crate::persist::{InstanceLock, Persistence};
use crate::store::VersionStore;
use crate::viewfs::{TicketRuntime, ViewEngine};
use parking_lot::{Mutex, RwLock};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InputBoundResponse<T> {
    input_digest: Digest,
    result: T,
}
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub storage_root: PathBuf,
    pub import_root: PathBuf,
    pub main_branch: BranchId,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct RecoveryReport {
    pub mode: FsMode,
    pub mount_seq: u64,
    pub branches: usize,
    pub aborted_open_tickets: usize,
    pub repaired_receipts: usize,
    pub journal_records: usize,
    pub damage: Vec<String>,
}

#[derive(Debug)]
struct RuntimeState {
    branches: HashMap<BranchId, BranchHead>,
    branch_meta: HashMap<BranchId, BranchMeta>,
    txs: HashMap<TxId, TxMeta>,
    tickets: HashMap<TicketId, Arc<TicketRuntime>>,
    ticket_meta: HashMap<TicketId, TicketMeta>,
    candidates: HashMap<CandidateId, CandidateMeta>,
    receipts: HashMap<CandidateId, PublishReceipt>,
    publication_index: HashMap<(TxId, TicketId), GenerationId>,
}

impl RuntimeState {
    fn empty() -> Self {
        Self {
            branches: HashMap::new(),
            branch_meta: HashMap::new(),
            txs: HashMap::new(),
            tickets: HashMap::new(),
            ticket_meta: HashMap::new(),
            candidates: HashMap::new(),
            receipts: HashMap::new(),
            publication_index: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct Inner {
    persist: Persistence,
    store: VersionStore,
    views: Arc<ViewEngine>,
    _lock: InstanceLock,
    superblock: RwLock<Superblock>,
    state: RwLock<RuntimeState>,
    branch_locks: Mutex<HashMap<BranchId, Arc<Mutex<()>>>>,
    ticket_locks: Mutex<HashMap<TicketId, Arc<Mutex<()>>>>,
    tx_locks: Mutex<HashMap<TxId, Arc<Mutex<()>>>>,
    request_locks: Mutex<HashMap<RequestId, Arc<Mutex<()>>>>,
    recovery: RwLock<RecoveryReport>,
}

#[derive(Debug, Clone)]
pub struct CensorFs {
    inner: Arc<Inner>,
}

impl CensorFs {
    pub fn initialize(options: InitOptions) -> Result<Self> {
        Self::initialize_with_fault(options, Arc::new(FaultInjector::disabled()))
    }

    pub fn initialize_with_fault(options: InitOptions, fault: Arc<FaultInjector>) -> Result<Self> {
        if options.storage_root.exists() && options.storage_root.read_dir()?.next().is_some() {
            return Err(CensorFsError::new(
                ErrorCode::AlreadyExistsDifferent,
                "storage root must be empty",
            ));
        }
        if !options.import_root.is_dir() {
            return Err(CensorFsError::bad_request("import root must be a directory"));
        }
        let persist = Persistence::new(&options.storage_root, fault);
        persist.initialize_layout()?;
        persist.validate_backing_filesystem()?;
        let lock = persist.acquire_lock()?;
        let store = VersionStore::new(persist.clone());

        let generation_id = GenerationId::new();
        let mut entries = BTreeMap::new();
        entries.insert(
            LogicalPath::root(),
            ManifestEntry {
                path: LogicalPath::root(),
                kind: EntryKind::Directory,
                mode: 0o755,
                size: 0,
                atime_ns: 0,
                mtime_ns: 0,
                object_id: None,
                content_digest: None,
            },
        );
        for item in WalkDir::new(&options.import_root)
            .follow_links(false)
            .min_depth(1)
        {
            let item = item.map_err(|e| CensorFsError::new(ErrorCode::IoError, e.to_string()))?;
            let relative = item
                .path()
                .strip_prefix(&options.import_root)
                .map_err(|_| CensorFsError::bad_request("import path escaped root"))?;
            let logical = path_to_logical(relative)?;
            let metadata = fs::symlink_metadata(item.path())?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() || !(file_type.is_file() || file_type.is_dir()) {
                return Err(CensorFsError::new(
                    ErrorCode::Unsupported,
                    format!("unsupported import entry {}", item.path().display()),
                ));
            }
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o777
            };
            #[cfg(not(unix))]
            let mode = if file_type.is_dir() { 0o755 } else { 0o644 };
            if file_type.is_dir() {
                entries.insert(
                    logical.clone(),
                    ManifestEntry {
                        path: logical,
                        kind: EntryKind::Directory,
                        mode,
                        size: 0,
                        atime_ns: 0,
                        mtime_ns: 0,
                        object_id: None,
                        content_digest: None,
                    },
                );
            } else {
                let data = fs::read(item.path())?;
                let (object_id, digest) = store.write_object(&data)?;
                entries.insert(
                    logical.clone(),
                    ManifestEntry {
                        path: logical,
                        kind: EntryKind::File,
                        mode,
                        size: data.len() as u64,
                        atime_ns: 0,
                        mtime_ns: 0,
                        object_id: Some(object_id),
                        content_digest: Some(digest),
                    },
                );
            }
        }
        let manifest = Manifest {
            generation_id,
            entries,
        };
        let (manifest_id, manifest_digest) = store.write_manifest(&manifest)?;
        let generation = GenerationMeta {
            generation_id,
            manifest_id,
            parents: Vec::new(),
            source_ticket: None,
            source_candidate: None,
            kind: GenerationKind::Initial,
            rollback_target: None,
            manifest_digest,
            entry_count: manifest.entries.len() as u64,
        };
        store.write_generation(&generation)?;

        let branch_dir = persist.branch_dir(&options.main_branch);
        fs::create_dir_all(&branch_dir)?;
        let branch_meta = BranchMeta {
            branch_id: options.main_branch.clone(),
            created_from: generation_id,
            created_by_request: None,
        };
        persist.write_record(
            &branch_dir.join("branch.meta"),
            RecordKind::BranchMeta,
            &branch_meta,
            true,
        )?;
        let head = BranchHead {
            generation_id,
            head_seq: 0,
        };
        persist.write_record(
            &branch_dir.join("head.a"),
            RecordKind::HeadSlot,
            &head,
            true,
        )?;
        persist.sync_dir(&branch_dir)?;

        let superblock = Superblock {
            instance_id: InstanceId::new(),
            format_version: 1,
            mount_seq: 1,
            mode: FsMode::ReadWrite,
        };
        persist.write_record(
            &persist.path("superblock/slot.a"),
            RecordKind::Superblock,
            &superblock,
            true,
        )?;
        persist.sync_dir(&persist.path("superblock"))?;

        let views = Arc::new(ViewEngine::new(store.clone(), persist.clone()));
        let mut state = RuntimeState::empty();
        state.branches.insert(options.main_branch.clone(), head);
        state.branch_meta.insert(options.main_branch, branch_meta);
        let recovery = RecoveryReport {
            mode: FsMode::ReadWrite,
            mount_seq: 1,
            branches: 1,
            aborted_open_tickets: 0,
            repaired_receipts: 0,
            journal_records: 0,
            damage: Vec::new(),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                persist,
                store,
                views,
                _lock: lock,
                superblock: RwLock::new(superblock),
                state: RwLock::new(state),
                branch_locks: Mutex::new(HashMap::new()),
                ticket_locks: Mutex::new(HashMap::new()),
                tx_locks: Mutex::new(HashMap::new()),
                request_locks: Mutex::new(HashMap::new()),
                recovery: RwLock::new(recovery),
            }),
        })
    }

    pub fn open(storage_root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_fault(storage_root, Arc::new(FaultInjector::disabled()))
    }

    pub fn open_with_fault(
        storage_root: impl Into<PathBuf>,
        fault: Arc<FaultInjector>,
    ) -> Result<Self> {
        let persist = Persistence::new(storage_root, fault);
        persist.initialize_layout()?;
        persist.validate_backing_filesystem()?;
        let lock = persist.acquire_lock()?;
        let (mut superblock, active_super_slot) = read_superblock(&persist)?;
        let journal = persist.recover_journal()?;
        let publish_requests: HashMap<CandidateId, RequestId> = journal
            .iter()
            .filter(|record| {
                matches!(
                    record.kind,
                    JournalKind::HeadSwitchPrepare | JournalKind::HeadSwitchCommit
                )
            })
            .filter_map(|record| {
                CandidateId::parse(&record.target_id)
                    .ok()
                    .map(|candidate| (candidate, record.operation_id))
            })
            .collect();
        let store = VersionStore::new(persist.clone());
        let mut state = RuntimeState::empty();
        let mut damage = Vec::new();
        let mut aborted = 0usize;
        let mut repaired = 0usize;

        for entry in fs::read_dir(persist.path("branches"))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let meta_path = entry.path().join("branch.meta");
            let meta: BranchMeta = match persist.read_record(&meta_path, RecordKind::BranchMeta) {
                Ok(value) => value,
                Err(error) => {
                    damage.push(format!("{}: {error}", meta_path.display()));
                    continue;
                }
            };
            match read_best_head(&persist, &meta.branch_id) {
                Ok((head, _)) => {
                    if let Err(error) = store.load_generation(head.generation_id) {
                        damage.push(format!(
                            "branch {} head is invalid: {error}",
                            meta.branch_id
                        ));
                    }
                    state.branches.insert(meta.branch_id.clone(), head);
                    state.branch_meta.insert(meta.branch_id.clone(), meta);
                }
                Err(error) => damage.push(format!(
                    "branch {} has no valid head: {error}",
                    meta.branch_id
                )),
            }
        }

        for entry in fs::read_dir(persist.path("txs"))? {
            let path = entry?.path();
            if path.extension().and_then(|v| v.to_str()) != Some("meta") {
                continue;
            }
            match persist.read_record::<TxMeta>(&path, RecordKind::Tx) {
                Ok(meta) => {
                    state.txs.insert(meta.tx_id, meta);
                }
                Err(error) => damage.push(format!("{}: {error}", path.display())),
            }
        }

        for entry in fs::read_dir(persist.path("candidates"))? {
            let path = entry?.path();
            if path.extension().and_then(|v| v.to_str()) != Some("meta") {
                continue;
            }
            match persist.read_record::<CandidateMeta>(&path, RecordKind::Candidate) {
                Ok(meta) => {
                    if let Err(error) = store.load_generation(meta.result_generation) {
                        damage.push(format!(
                            "candidate {} result invalid: {error}",
                            meta.candidate_id
                        ));
                    } else {
                        state.candidates.insert(meta.candidate_id, meta);
                    }
                }
                Err(error) => damage.push(format!("{}: {error}", path.display())),
            }
        }

        let candidate_by_ticket: HashMap<TicketId, CandidateId> = state
            .candidates
            .values()
            .filter_map(|candidate| {
                candidate
                    .ticket_id
                    .map(|ticket| (ticket, candidate.candidate_id))
            })
            .collect();

        for entry in fs::read_dir(persist.path("tickets"))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("ticket.meta");
            if !path.exists() {
                continue;
            }
            let mut meta: TicketMeta = match persist.read_record(&path, RecordKind::Ticket) {
                Ok(value) => value,
                Err(error) => {
                    damage.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            if matches!(meta.state, TicketState::Open | TicketState::Freezing) {
                if let Some(candidate_id) = meta
                    .candidate_id
                    .or_else(|| candidate_by_ticket.get(&meta.ticket_id).copied())
                {
                    meta.candidate_id = Some(candidate_id);
                    meta.state = TicketState::Prepared;
                    persist.write_record(&path, RecordKind::Ticket, &meta, false)?;
                } else {
                    meta.state = TicketState::Aborted;
                    persist.write_record(&path, RecordKind::Ticket, &meta, false)?;
                    aborted += 1;
                }
            }
            if meta.state == TicketState::Prepared
                && meta
                    .candidate_id
                    .is_none_or(|candidate_id| !state.candidates.contains_key(&candidate_id))
            {
                damage.push(format!(
                    "ticket {} is prepared without a valid candidate",
                    meta.ticket_id
                ));
            }
            state.ticket_meta.insert(meta.ticket_id, meta);
        }

        let publish_root = persist.path("publish");
        for branch_dir in fs::read_dir(&publish_root)? {
            let branch_dir = branch_dir?;
            if !branch_dir.file_type()?.is_dir() {
                continue;
            }
            for receipt in fs::read_dir(branch_dir.path())? {
                let path = receipt?.path();
                if path.extension().and_then(|v| v.to_str()) != Some("receipt") {
                    continue;
                }
                match persist.read_record::<PublishReceipt>(&path, RecordKind::Receipt) {
                    Ok(value) => {
                        if let Some(ticket) = value.ticket_id {
                            state
                                .publication_index
                                .insert((value.tx_id, ticket), value.new_generation);
                        }
                        state.receipts.insert(value.candidate_id, value);
                    }
                    Err(error) => damage.push(format!("{}: {error}", path.display())),
                }
            }
        }

        for receipt in state.receipts.values().cloned().collect::<Vec<_>>() {
            let Some(candidate) = state.candidates.get_mut(&receipt.candidate_id) else {
                damage.push(format!("receipt {} has no candidate", receipt.candidate_id));
                continue;
            };
            if candidate.result_generation != receipt.new_generation
                || candidate.branch_id != receipt.branch_id
            {
                damage.push(format!(
                    "receipt {} does not match its candidate",
                    receipt.candidate_id
                ));
                continue;
            }
            if candidate.state != CandidateState::Published {
                candidate.state = CandidateState::Published;
                persist.write_record(
                    &persist.candidate_path(candidate.candidate_id),
                    RecordKind::Candidate,
                    candidate,
                    false,
                )?;
            }
            if let Some(ticket_id) = receipt.ticket_id {
                if let Some(ticket) = state.ticket_meta.get_mut(&ticket_id) {
                    if ticket.state != TicketState::Published {
                        ticket.state = TicketState::Published;
                        persist.write_record(
                            &persist.ticket_meta_path(ticket_id),
                            RecordKind::Ticket,
                            ticket,
                            false,
                        )?;
                    }
                }
            }
        }

        for (branch, head) in state.branches.clone() {
            let candidate = state
                .candidates
                .values()
                .find(|c| c.branch_id == branch && c.result_generation == head.generation_id)
                .cloned();
            if let Some(mut candidate) = candidate {
                if !state.receipts.contains_key(&candidate.candidate_id) {
                    let receipt = PublishReceipt {
                        branch_id: branch.clone(),
                        head_seq: head.head_seq,
                        old_generation: candidate.base_generation,
                        new_generation: candidate.result_generation,
                        candidate_id: candidate.candidate_id,
                        ticket_id: candidate.ticket_id,
                        tx_id: candidate.tx_id,
                        decision_id: "recovered".into(),
                        request_id: publish_requests
                            .get(&candidate.candidate_id)
                            .copied()
                            .unwrap_or_else(RequestId::new),
                    };
                    let path = persist.receipt_path(&branch, head.head_seq);
                    persist.write_record(&path, RecordKind::Receipt, &receipt, true)?;
                    candidate.state = CandidateState::Published;
                    persist.write_record(
                        &persist.candidate_path(candidate.candidate_id),
                        RecordKind::Candidate,
                        &candidate,
                        false,
                    )?;
                    if let Some(ticket_id) = candidate.ticket_id {
                        if let Some(ticket) = state.ticket_meta.get_mut(&ticket_id) {
                            ticket.state = TicketState::Published;
                            persist.write_record(
                                &persist.ticket_meta_path(ticket_id),
                                RecordKind::Ticket,
                                ticket,
                                false,
                            )?;
                            state
                                .publication_index
                                .insert((ticket.tx_id, ticket_id), candidate.result_generation);
                        }
                    }
                    state
                        .candidates
                        .insert(candidate.candidate_id, candidate.clone());
                    state.receipts.insert(candidate.candidate_id, receipt);
                    repaired += 1;
                }
            }
        }

        superblock.mount_seq = superblock
            .mount_seq
            .checked_add(1)
            .ok_or_else(|| CensorFsError::corrupt("superblock mount sequence overflow"))?;
        if !damage.is_empty() {
            superblock.mode = FsMode::ReadOnly;
        }
        let next_slot = if active_super_slot == 'a' { 'b' } else { 'a' };
        persist.write_record(
            &persist.path(format!("superblock/slot.{next_slot}")),
            RecordKind::Superblock,
            &superblock,
            false,
        )?;
        let views = Arc::new(ViewEngine::new(store.clone(), persist.clone()));
        let recovery = RecoveryReport {
            mode: superblock.mode,
            mount_seq: superblock.mount_seq,
            branches: state.branches.len(),
            aborted_open_tickets: aborted,
            repaired_receipts: repaired,
            journal_records: journal.len(),
            damage,
        };
        let fs = Self {
            inner: Arc::new(Inner {
                persist,
                store,
                views,
                _lock: lock,
                superblock: RwLock::new(superblock),
                state: RwLock::new(state),
                branch_locks: Mutex::new(HashMap::new()),
                ticket_locks: Mutex::new(HashMap::new()),
                tx_locks: Mutex::new(HashMap::new()),
                request_locks: Mutex::new(HashMap::new()),
                recovery: RwLock::new(recovery),
            }),
        };
        fs.rebuild_stable_request_results()?;
        Ok(fs)
    }

    pub fn persistence(&self) -> &Persistence {
        &self.inner.persist
    }
    pub fn view_engine(&self) -> &ViewEngine {
        &self.inner.views
    }
    pub fn view_engine_arc(&self) -> Arc<ViewEngine> {
        Arc::clone(&self.inner.views)
    }
    pub fn recovery_report(&self) -> RecoveryReport {
        self.inner.recovery.read().clone()
    }
    pub fn superblock(&self) -> Superblock {
        self.inner.superblock.read().clone()
    }

    fn ensure_writable(&self) -> Result<()> {
        if self.inner.superblock.read().mode == FsMode::ReadOnly {
            return Err(CensorFsError::new(
                ErrorCode::ReadOnly,
                "instance is read-only",
            ));
        }
        Ok(())
    }

    fn idempotent<T, F>(&self, request: RequestId, operation: &str, action: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned + Clone,
        F: FnOnce() -> Result<T>,
    {
        let request_lock = self.request_lock(request);
        let _guard = request_lock.lock();
        if let Some(result) = self.inner.persist.load_response(request, operation)? {
            return Ok(result);
        }
        let result = action()?;
        self.inner
            .persist
            .save_response(request, operation, &result)?;
        Ok(result)
    }

    pub(crate) fn idempotent_with_input<I, T, F>(
        &self,
        request: RequestId,
        operation: &str,
        input: &I,
        action: F,
    ) -> Result<T>
    where
        I: Serialize,
        T: Serialize + DeserializeOwned + Clone,
        F: FnOnce() -> Result<T>,
    {
        let input_digest = *blake3::hash(&codec::serialize(input)?).as_bytes();
        let request_lock = self.request_lock(request);
        let _guard = request_lock.lock();
        if let Some(saved) = self
            .inner
            .persist
            .load_response::<InputBoundResponse<T>>(request, operation)?
        {
            if saved.input_digest != input_digest {
                return Err(CensorFsError::new(
                    ErrorCode::AlreadyExistsDifferent,
                    "request_id was already used with different input",
                ));
            }
            return Ok(saved.result);
        }
        let result = action()?;
        self.inner.persist.save_response(
            request,
            operation,
            &InputBoundResponse {
                input_digest,
                result: result.clone(),
            },
        )?;
        Ok(result)
    }

    fn journal<T: Serialize>(
        &self,
        request: RequestId,
        kind: JournalKind,
        target: String,
        payload: &T,
    ) -> Result<()> {
        let payload_digest = *blake3::hash(&codec::serialize(payload)?).as_bytes();
        self.inner.persist.append_journal(JournalRecord {
            seq: 0,
            operation_id: request,
            kind,
            target_id: target,
            payload_digest,
        })?;
        Ok(())
    }

    pub fn begin_tx(&self, request: RequestId, actor_id: impl Into<String>) -> Result<TxMeta> {
        self.ensure_writable()?;
        let actor_id = actor_id.into();
        self.idempotent(request, "begin_tx", || {
            if let Some(existing) = self
                .inner
                .state
                .read()
                .txs
                .values()
                .find(|t| t.created_by_request == request)
                .cloned()
            {
                return Ok(existing);
            }
            let tx = TxMeta {
                tx_id: TxId::new(),
                actor_id,
                state: TxState::Open,
                tickets: Vec::new(),
                created_by_request: request,
                terminal_request: None,
            };
            self.inner.persist.write_record(
                &self.inner.persist.tx_path(tx.tx_id),
                RecordKind::Tx,
                &tx,
                true,
            )?;
            self.inner.state.write().txs.insert(tx.tx_id, tx.clone());
            Ok(tx)
        })
    }

    pub fn close_tx(&self, request: RequestId, tx_id: TxId) -> Result<TxMeta> {
        self.ensure_writable()?;
        self.idempotent(request, "close_tx", || {
            let tx_lock = self.tx_lock(tx_id);
            let _guard = tx_lock.lock();
            let mut state = self.inner.state.write();
            let tx = state
                .txs
                .get(&tx_id)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("tx", tx_id))?;
            if tx.state != TxState::Open {
                return Err(CensorFsError::new(ErrorCode::StateChanged, "tx is not open"));
            }
            if tx.tickets.iter().any(|id| {
                state.ticket_meta.get(id).is_some_and(|t| {
                    !matches!(t.state, TicketState::Published | TicketState::Aborted)
                })
            }) {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "tx still contains a non-terminal ticket",
                ));
            }
            if state.candidates.values().any(|candidate| {
                candidate.tx_id == tx_id && candidate.state == CandidateState::Prepared
            }) {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "tx still contains a prepared candidate",
                ));
            }
            let mut updated = tx;
            updated.state = TxState::Closed;
            updated.terminal_request = Some(request);
            self.inner.persist.write_record(
                &self.inner.persist.tx_path(tx_id),
                RecordKind::Tx,
                &updated,
                false,
            )?;
            state.txs.insert(tx_id, updated.clone());
            Ok(updated)
        })
    }

    pub fn abort_tx(&self, request: RequestId, tx_id: TxId) -> Result<TxMeta> {
        self.ensure_writable()?;
        self.idempotent(request, "abort_tx", || {
            let tx_lock = self.tx_lock(tx_id);
            let _guard = tx_lock.lock();
            let tx = self
                .inner
                .state
                .read()
                .txs
                .get(&tx_id)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("tx", tx_id))?;
            if tx.state != TxState::Open {
                return Err(CensorFsError::new(ErrorCode::StateChanged, "tx is not open"));
            }
            for ticket in tx.tickets.clone() {
                if self
                    .inner
                    .state
                    .read()
                    .ticket_meta
                    .get(&ticket)
                    .is_some_and(|t| {
                        t.state != TicketState::Published && t.state != TicketState::Aborted
                    })
                {
                    self.abort_ticket_inner(RequestId::new(), ticket)?;
                }
            }
            let rollback_candidates: Vec<_> = self
                .inner
                .state
                .read()
                .candidates
                .values()
                .filter(|candidate| {
                    candidate.tx_id == tx_id
                        && candidate.ticket_id.is_none()
                        && candidate.state == CandidateState::Prepared
                })
                .cloned()
                .collect();
            for candidate in rollback_candidates {
                let branch_lock = self.branch_lock(&candidate.branch_id);
                let _branch_guard = branch_lock.lock();
                let Some(mut candidate) = self
                    .inner
                    .state
                    .read()
                    .candidates
                    .get(&candidate.candidate_id)
                    .cloned()
                else {
                    continue;
                };
                if candidate.state != CandidateState::Prepared {
                    continue;
                }
                candidate.state = CandidateState::Aborted;
                self.inner.persist.write_record(
                    &self.inner.persist.candidate_path(candidate.candidate_id),
                    RecordKind::Candidate,
                    &candidate,
                    false,
                )?;
                self.inner
                    .state
                    .write()
                    .candidates
                    .insert(candidate.candidate_id, candidate);
            }
            let mut updated = tx;
            updated.state = TxState::Aborted;
            updated.terminal_request = Some(request);
            self.inner.persist.write_record(
                &self.inner.persist.tx_path(tx_id),
                RecordKind::Tx,
                &updated,
                false,
            )?;
            self.inner.state.write().txs.insert(tx_id, updated.clone());
            Ok(updated)
        })
    }

    pub fn create_branch(
        &self,
        request: RequestId,
        branch_id: BranchId,
        base: GenerationId,
    ) -> Result<BranchHead> {
        self.ensure_writable()?;
        self.idempotent(request, "create_branch", || {
            let branch_lock = self.branch_lock(&branch_id);
            let _guard = branch_lock.lock();
            self.inner.store.load_generation(base)?;
            if let Some(head) = self.inner.state.read().branches.get(&branch_id).cloned() {
                let meta: BranchMeta = self.inner.persist.read_record(
                    &self
                        .inner
                        .persist
                        .branch_dir(&branch_id)
                        .join("branch.meta"),
                    RecordKind::BranchMeta,
                )?;
                if meta.created_from == base {
                    return Ok(head);
                }
                return Err(CensorFsError::new(
                    ErrorCode::AlreadyExistsDifferent,
                    "branch exists with a different base",
                ));
            }
            let final_dir = self.inner.persist.branch_dir(&branch_id);
            let temp_dir = self.inner.persist.path("tmp").join(format!(
                "branch.{}.{}",
                branch_id.storage_name(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&temp_dir)?;
            let meta = BranchMeta {
                branch_id: branch_id.clone(),
                created_from: base,
                created_by_request: Some(request),
            };
            self.inner.persist.write_record(
                &temp_dir.join("branch.meta"),
                RecordKind::BranchMeta,
                &meta,
                true,
            )?;
            let head = BranchHead {
                generation_id: base,
                head_seq: 0,
            };
            self.inner.persist.write_record(
                &temp_dir.join("head.a"),
                RecordKind::HeadSlot,
                &head,
                true,
            )?;
            self.inner.persist.sync_dir(&temp_dir)?;
            fs::rename(&temp_dir, &final_dir)?;
            self.inner
                .persist
                .sync_dir(&self.inner.persist.path("branches"))?;
            self.journal(
                request,
                JournalKind::BranchCreated,
                branch_id.to_string(),
                &head,
            )?;
            let mut state = self.inner.state.write();
            state.branch_meta.insert(branch_id.clone(), meta);
            state.branches.insert(branch_id, head.clone());
            Ok(head)
        })
    }

    pub fn branch_head(&self, branch: &BranchId) -> Result<BranchHead> {
        read_best_head(&self.inner.persist, branch).map(|(head, _)| head)
    }

    pub fn branches(&self) -> Vec<BranchInfo> {
        let state = self.inner.state.read();
        let mut result: Vec<_> = state
            .branches
            .iter()
            .filter_map(|(branch_id, head)| {
                state.branch_meta.get(branch_id).map(|meta| BranchInfo {
                    branch_id: branch_id.clone(),
                    head: head.clone(),
                    created_from: meta.created_from,
                })
            })
            .collect();
        result.sort_by(|left, right| left.branch_id.cmp(&right.branch_id));
        result
    }

    pub fn begin_exploration(
        &self,
        request: RequestId,
        actor_id: impl Into<String>,
        branch: BranchId,
    ) -> Result<ExplorationResult> {
        self.ensure_writable()?;
        let actor_id = actor_id.into();
        self.idempotent(request, "begin_exploration", || {
            let tx = self.begin_tx(request.derive("begin_tx"), actor_id)?;
            let ticket =
                self.begin_ticket(request.derive("begin_ticket"), tx.tx_id, branch, None)?;
            Ok(ExplorationResult { tx, ticket })
        })
    }

    pub fn commit_exploration(
        &self,
        request: RequestId,
        ticket_id: TicketId,
        message: impl Into<String>,
        timeout: Duration,
    ) -> Result<CommitResult> {
        self.ensure_writable()?;
        let message = message.into();
        self.idempotent(request, "commit_exploration", || {
            let original = self.ticket(ticket_id)?;
            self.ensure_single_ticket_exploration(&original)?;
            let prepared = self.prepare_ticket(request.derive("prepare"), ticket_id, timeout)?;
            let receipt = self.publish(
                request.derive("publish"),
                prepared.candidate.candidate_id,
                message,
                BranchHead {
                    generation_id: original.base_generation,
                    head_seq: original.expected_head_seq,
                },
            )?;
            let tx = self.close_tx(request.derive("close_tx"), original.tx_id)?;
            Ok(CommitResult {
                ticket: self.ticket(ticket_id)?,
                candidate: self.candidate(prepared.candidate.candidate_id)?,
                generation: prepared.generation,
                receipt,
                tx,
            })
        })
    }

    pub fn abort_exploration(
        &self,
        request: RequestId,
        ticket_id: TicketId,
    ) -> Result<AbortResult> {
        self.ensure_writable()?;
        self.idempotent(request, "abort_exploration", || {
            let original = self.ticket(ticket_id)?;
            self.ensure_single_ticket_exploration(&original)?;
            let ticket = self.abort_ticket(request.derive("abort_ticket"), ticket_id)?;
            let tx = self.abort_tx(request.derive("abort_tx"), original.tx_id)?;
            Ok(AbortResult { ticket, tx })
        })
    }

    fn ensure_single_ticket_exploration(&self, ticket: &TicketMeta) -> Result<()> {
        let tx = self.tx(ticket.tx_id)?;
        if tx.tickets.len() != 1 || tx.tickets[0] != ticket.ticket_id {
            return Err(CensorFsError::bad_request(
                "high-level exploration requires exactly one ticket in its transaction",
            ));
        }
        Ok(())
    }

    pub fn begin_ticket(
        &self,
        request: RequestId,
        tx_id: TxId,
        branch: BranchId,
        expected: Option<BranchHead>,
    ) -> Result<TicketMeta> {
        self.ensure_writable()?;
        let request_lock = self.request_lock(request);
        let _guard = request_lock.lock();
        if let Some(saved) = self
            .inner
            .persist
            .load_response::<TicketMeta>(request, "begin_ticket")?
        {
            return Ok(saved);
        }
        if let Some(saved) = self
            .inner
            .state
            .read()
            .ticket_meta
            .values()
            .find(|ticket| ticket.created_by_request == request)
            .cloned()
        {
            let original = original_ticket_result(&saved);
            self.inner
                .persist
                .save_response(request, "begin_ticket", &original)?;
            return Ok(original);
        }
        let tx_lock = self.tx_lock(tx_id);
        let _tx_guard = tx_lock.lock();
        let tx = self
            .inner
            .state
            .read()
            .txs
            .get(&tx_id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("tx", tx_id))?;
        if tx.state != TxState::Open {
            return Err(CensorFsError::new(ErrorCode::StateChanged, "tx is not open"));
        }
        let head = self.branch_head(&branch)?;
        if expected.as_ref().is_some_and(|v| v != &head) {
            return Err(CensorFsError::new(
                ErrorCode::HeadChanged,
                format!("current head is {}/{}", head.generation_id, head.head_seq),
            ));
        }
        let ticket_id = TicketId::new();
        let ticket = TicketMeta {
            ticket_id,
            tx_id,
            branch_id: branch,
            base_generation: head.generation_id,
            expected_head_seq: head.head_seq,
            state: TicketState::Open,
            candidate_id: None,
            created_by_request: request,
            terminal_request: None,
        };
        let ticket_dir = self.inner.persist.ticket_dir(ticket_id);
        fs::create_dir_all(ticket_dir.join("upper"))?;
        File::create(ticket_dir.join("delta.log"))?.sync_all()?;
        self.inner.persist.write_record(
            &ticket_dir.join("ticket.meta"),
            RecordKind::Ticket,
            &ticket,
            true,
        )?;
        self.inner.persist.sync_dir(&ticket_dir)?;
        let runtime = Arc::new(TicketRuntime::new(ticket.clone(), &ticket_dir)?);
        {
            let mut state = self.inner.state.write();
            state.ticket_meta.insert(ticket_id, ticket.clone());
            state.tickets.insert(ticket_id, runtime);
            let tx = state.txs.get_mut(&tx_id).unwrap();
            tx.tickets.push(ticket_id);
            self.inner.persist.write_record(
                &self.inner.persist.tx_path(tx_id),
                RecordKind::Tx,
                tx,
                false,
            )?;
        }
        self.inner
            .persist
            .save_response(request, "begin_ticket", &ticket)?;
        Ok(ticket)
    }

    fn new_view(
        &self,
        kind: ViewKind,
        generation: GenerationId,
        ticket_id: Option<TicketId>,
        owner_uid: u32,
        owner_gid: u32,
        can_write: bool,
        runtime: Option<Arc<TicketRuntime>>,
    ) -> Result<ViewHandle> {
        let handle = ViewHandle {
            view_id: ViewId::new(),
            kind,
            generation_id: generation,
            ticket_id,
            owner_uid,
            owner_gid,
            can_write,
            mount_epoch: self.inner.superblock.read().mount_seq,
            state: ViewState::Created,
        };
        self.inner.views.register_view(handle.clone(), runtime)?;
        Ok(handle)
    }

    pub fn open_branch_view(
        &self,
        branch: &BranchId,
        owner_uid: u32,
        owner_gid: u32,
    ) -> Result<ViewHandle> {
        let head = self.branch_head(branch)?;
        self.new_view(
            ViewKind::Stable,
            head.generation_id,
            None,
            owner_uid,
            owner_gid,
            false,
            None,
        )
    }

    pub fn open_generation_view(
        &self,
        generation: GenerationId,
        owner_uid: u32,
        owner_gid: u32,
    ) -> Result<ViewHandle> {
        self.inner.store.load_generation(generation)?;
        self.new_view(
            ViewKind::Historical,
            generation,
            None,
            owner_uid,
            owner_gid,
            false,
            None,
        )
    }

    pub fn open_candidate_view(
        &self,
        candidate: CandidateId,
        owner_uid: u32,
        owner_gid: u32,
    ) -> Result<ViewHandle> {
        let candidate = self
            .inner
            .state
            .read()
            .candidates
            .get(&candidate)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("candidate", candidate))?;
        self.new_view(
            ViewKind::Candidate,
            candidate.result_generation,
            None,
            owner_uid,
            owner_gid,
            false,
            None,
        )
    }

    pub fn open_ticket_view(
        &self,
        ticket_id: TicketId,
        owner_uid: u32,
        owner_gid: u32,
    ) -> Result<ViewHandle> {
        let ticket = self
            .inner
            .state
            .read()
            .ticket_meta
            .get(&ticket_id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("ticket", ticket_id))?;
        if ticket.state != TicketState::Open {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                "only an OPEN ticket can create a writable view",
            ));
        }
        let runtime = self
            .inner
            .state
            .read()
            .tickets
            .get(&ticket_id)
            .cloned()
            .ok_or_else(|| {
                CensorFsError::new(ErrorCode::StateChanged, "ticket runtime is unavailable")
            })?;
        self.new_view(
            ViewKind::Ticket,
            ticket.base_generation,
            Some(ticket_id),
            owner_uid,
            owner_gid,
            true,
            Some(runtime),
        )
    }

    pub fn close_view(&self, view_id: ViewId, caller_uid: u32) -> Result<ViewHandle> {
        let view = self.inner.views.view(view_id)?;
        if caller_uid != 0 && caller_uid != view.owner_uid {
            return Err(CensorFsError::new(
                ErrorCode::AccessDenied,
                "view belongs to another uid",
            ));
        }
        self.inner.views.close(view_id)?;
        let mut closed = view;
        closed.state = ViewState::Closed;
        Ok(closed)
    }

    pub fn prepare_ticket(
        &self,
        request: RequestId,
        ticket_id: TicketId,
        timeout: Duration,
    ) -> Result<PrepareResult> {
        self.ensure_writable()?;
        self.idempotent(request, "prepare_ticket", || {
            let ticket_lock = self.ticket_lock(ticket_id);
            let _guard = ticket_lock.lock();
            if let Some(candidate) = self
                .inner
                .state
                .read()
                .candidates
                .values()
                .find(|c| c.created_by_request == request)
                .cloned()
            {
                let (generation, _) = self
                    .inner
                    .store
                    .load_generation(candidate.result_generation)?;
                return Ok(PrepareResult {
                    candidate,
                    generation,
                });
            }
            let runtime = self
                .inner
                .state
                .read()
                .tickets
                .get(&ticket_id)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("active ticket", ticket_id))?;
            let overlay = match runtime.freeze(timeout) {
                Ok(value) => value,
                Err(error) => return Err(error),
            };
            let meta = runtime.meta.read().clone();
            let candidate_id = CandidateId::new();
            let generation = match self.inner.store.build_generation(
                meta.base_generation,
                &overlay,
                runtime.upper(),
                Some(ticket_id),
                candidate_id,
                GenerationKind::Normal,
                None,
            ) {
                Ok(value) => value,
                Err(error) => {
                    runtime.meta.write().state = TicketState::Open;
                    return Err(error);
                }
            };
            let changed_paths_digest = *blake3::hash(&codec::serialize(&overlay)?).as_bytes();
            let candidate = CandidateMeta {
                candidate_id,
                ticket_id: Some(ticket_id),
                tx_id: meta.tx_id,
                branch_id: meta.branch_id.clone(),
                base_generation: meta.base_generation,
                expected_head_seq: meta.expected_head_seq,
                result_generation: generation.generation_id,
                changed_paths_digest,
                state: CandidateState::Prepared,
                rollback_target: None,
                created_by_request: request,
            };
            self.inner.persist.write_record(
                &self.inner.persist.candidate_path(candidate_id),
                RecordKind::Candidate,
                &candidate,
                true,
            )?;
            self.inner.persist.fault().checkpoint("candidate.durable")?;
            self.journal(
                request,
                JournalKind::CandidateReady,
                candidate_id.to_string(),
                &candidate,
            )?;
            {
                let mut ticket = runtime.meta.write();
                ticket.state = TicketState::Prepared;
                ticket.candidate_id = Some(candidate_id);
                self.inner.persist.write_record(
                    &self.inner.persist.ticket_meta_path(ticket_id),
                    RecordKind::Ticket,
                    &*ticket,
                    false,
                )?;
                self.inner
                    .state
                    .write()
                    .ticket_meta
                    .insert(ticket_id, ticket.clone());
            }
            self.inner
                .state
                .write()
                .candidates
                .insert(candidate_id, candidate.clone());
            Ok(PrepareResult {
                candidate,
                generation,
            })
        })
    }

    pub fn build_rollback_candidate(
        &self,
        request: RequestId,
        tx_id: TxId,
        branch: BranchId,
        target: GenerationId,
        expected: BranchHead,
    ) -> Result<PrepareResult> {
        self.ensure_writable()?;
        self.idempotent(request, "build_rollback_candidate", || {
            let tx_lock = self.tx_lock(tx_id);
            let _guard = tx_lock.lock();
            if let Some(candidate) = self
                .inner
                .state
                .read()
                .candidates
                .values()
                .find(|candidate| candidate.created_by_request == request)
                .cloned()
            {
                let (generation, _) = self
                    .inner
                    .store
                    .load_generation(candidate.result_generation)?;
                return Ok(PrepareResult {
                    candidate,
                    generation,
                });
            }
            let tx = self
                .inner
                .state
                .read()
                .txs
                .get(&tx_id)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("tx", tx_id))?;
            if tx.state != TxState::Open {
                return Err(CensorFsError::new(ErrorCode::StateChanged, "tx is not open"));
            }
            let current = self.branch_head(&branch)?;
            if current != expected {
                return Err(CensorFsError::new(
                    ErrorCode::HeadChanged,
                    format!(
                        "current head is {}/{}",
                        current.generation_id, current.head_seq
                    ),
                ));
            }
            self.inner.store.load_generation(target)?;
            let candidate_id = CandidateId::new();
            let generation = self.inner.store.clone_generation_contents(
                current.generation_id,
                target,
                candidate_id,
            )?;
            let (_, manifest) = self.inner.store.load_generation(target)?;
            let digest = *blake3::hash(&codec::serialize(&manifest.entries)?).as_bytes();
            let candidate = CandidateMeta {
                candidate_id,
                ticket_id: None,
                tx_id,
                branch_id: branch,
                base_generation: current.generation_id,
                expected_head_seq: current.head_seq,
                result_generation: generation.generation_id,
                changed_paths_digest: digest,
                state: CandidateState::Prepared,
                rollback_target: Some(target),
                created_by_request: request,
            };
            self.inner.persist.write_record(
                &self.inner.persist.candidate_path(candidate_id),
                RecordKind::Candidate,
                &candidate,
                true,
            )?;
            self.inner
                .persist
                .fault()
                .checkpoint("rollback_candidate.durable")?;
            self.journal(
                request,
                JournalKind::RollbackCandidateReady,
                candidate_id.to_string(),
                &candidate,
            )?;
            self.inner
                .state
                .write()
                .candidates
                .insert(candidate_id, candidate.clone());
            Ok(PrepareResult {
                candidate,
                generation,
            })
        })
    }

    pub fn check_merge(
        &self,
        source_branch: BranchId,
        target_branch: BranchId,
    ) -> Result<MergeCheckResult> {
        if source_branch == target_branch {
            return Err(CensorFsError::bad_request(
                "source and target branch must be different",
            ));
        }
        let source_head = self.branch_head(&source_branch)?;
        let target_head = self.branch_head(&target_branch)?;
        let plan = self
            .inner
            .store
            .plan_merge(source_head.generation_id, target_head.generation_id)?;
        Ok(MergeCheckResult {
            source_branch,
            target_branch,
            source_head,
            target_head,
            merge_base: plan.merge_base,
            conflicts: plan.conflicts,
            already_up_to_date: plan.already_up_to_date,
        })
    }

    pub fn build_merge_candidate(
        &self,
        request: RequestId,
        tx_id: TxId,
        check: MergeCheckResult,
    ) -> Result<MergePrepareResult> {
        self.ensure_writable()?;
        self.idempotent(request, "build_merge_candidate", || {
            if let Some(candidate) = self
                .inner
                .state
                .read()
                .candidates
                .values()
                .find(|candidate| candidate.created_by_request == request)
                .cloned()
            {
                let generation = self
                    .inner
                    .store
                    .load_generation_meta(candidate.result_generation)?;
                let intent = self.inner.persist.read_record(
                    &self.inner.persist.merge_intent_path(candidate.candidate_id),
                    RecordKind::MergeIntent,
                )?;
                return Ok(MergePrepareResult {
                    check,
                    intent,
                    candidate,
                    generation,
                });
            }
            if check.already_up_to_date {
                return Err(CensorFsError::new(
                    ErrorCode::AlreadyUpToDate,
                    "source and target already point to the same generation",
                ));
            }
            if !check.conflicts.is_empty() {
                return Err(CensorFsError::new(
                    ErrorCode::Conflict,
                    "merge contains path conflicts",
                ));
            }
            let tx = self.tx(tx_id)?;
            if tx.state != TxState::Open {
                return Err(CensorFsError::new(ErrorCode::StateChanged, "tx is not open"));
            }
            if self.branch_head(&check.source_branch)? != check.source_head
                || self.branch_head(&check.target_branch)? != check.target_head
            {
                return Err(CensorFsError::new(
                    ErrorCode::HeadChanged,
                    "source or target head changed while preparing merge",
                ));
            }
            let plan = self.inner.store.plan_merge(
                check.source_head.generation_id,
                check.target_head.generation_id,
            )?;
            if plan.merge_base != check.merge_base || !plan.conflicts.is_empty() {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "merge plan changed while preparing candidate",
                ));
            }
            let candidate_id = CandidateId::new();
            let generation = self.inner.store.write_merge_generation(
                check.target_head.generation_id,
                check.source_head.generation_id,
                plan.entries.clone(),
                candidate_id,
            )?;
            let changed_paths_digest = *blake3::hash(&codec::serialize(&plan.entries)?).as_bytes();
            let candidate = CandidateMeta {
                candidate_id,
                ticket_id: None,
                tx_id,
                branch_id: check.target_branch.clone(),
                base_generation: check.target_head.generation_id,
                expected_head_seq: check.target_head.head_seq,
                result_generation: generation.generation_id,
                changed_paths_digest,
                state: CandidateState::Prepared,
                rollback_target: None,
                created_by_request: request,
            };
            let intent = MergeIntent {
                candidate_id,
                source_branch: check.source_branch.clone(),
                target_branch: check.target_branch.clone(),
                source_head: check.source_head.clone(),
                target_head: check.target_head.clone(),
                merge_base: check.merge_base,
            };
            self.inner.persist.write_record(
                &self.inner.persist.merge_intent_path(candidate_id),
                RecordKind::MergeIntent,
                &intent,
                true,
            )?;
            self.inner.persist.write_record(
                &self.inner.persist.candidate_path(candidate_id),
                RecordKind::Candidate,
                &candidate,
                true,
            )?;
            self.journal(
                request,
                JournalKind::MergeCandidateReady,
                candidate_id.to_string(),
                &candidate,
            )?;
            self.inner
                .state
                .write()
                .candidates
                .insert(candidate_id, candidate.clone());
            Ok(MergePrepareResult {
                check,
                intent,
                candidate,
                generation,
            })
        })
    }

    pub fn merge_branches(
        &self,
        request: RequestId,
        actor_id: impl Into<String>,
        source_branch: BranchId,
        target_branch: BranchId,
        message: impl Into<String>,
        check_only: bool,
    ) -> Result<MergeResult> {
        if check_only {
            let check = self.check_merge(source_branch, target_branch)?;
            return Ok(MergeResult {
                check,
                prepared: None,
                receipt: None,
                tx: None,
            });
        }
        self.ensure_writable()?;
        let actor_id = actor_id.into();
        let message = message.into();
        self.idempotent(request, "merge_branches", || {
            let prepare_request = request.derive("prepare_merge");
            let check = if let Some(candidate) = self
                .inner
                .state
                .read()
                .candidates
                .values()
                .find(|candidate| candidate.created_by_request == prepare_request)
                .cloned()
            {
                let intent: MergeIntent = self.inner.persist.read_record(
                    &self.inner.persist.merge_intent_path(candidate.candidate_id),
                    RecordKind::MergeIntent,
                )?;
                if intent.source_branch != source_branch || intent.target_branch != target_branch {
                    return Err(CensorFsError::new(
                        ErrorCode::AlreadyExistsDifferent,
                        "request id was already used for a different merge",
                    ));
                }
                MergeCheckResult {
                    source_branch: intent.source_branch,
                    target_branch: intent.target_branch,
                    source_head: intent.source_head,
                    target_head: intent.target_head,
                    merge_base: intent.merge_base,
                    conflicts: Vec::new(),
                    already_up_to_date: false,
                }
            } else {
                self.check_merge(source_branch, target_branch)?
            };
            if check.already_up_to_date || !check.conflicts.is_empty() {
                return Ok(MergeResult {
                    check,
                    prepared: None,
                    receipt: None,
                    tx: None,
                });
            }
            let tx = self.begin_tx(request.derive("begin_tx"), actor_id)?;
            let prepared = self.build_merge_candidate(prepare_request, tx.tx_id, check.clone())?;
            let receipt = self.publish_merge(
                request.derive("publish_merge"),
                prepared.candidate.candidate_id,
                message,
                prepared.intent.clone(),
            )?;
            let tx = self.close_tx(request.derive("close_tx"), tx.tx_id)?;
            Ok(MergeResult {
                check,
                prepared: Some(prepared),
                receipt: Some(receipt),
                tx: Some(tx),
            })
        })
    }

    pub fn publish(
        &self,
        request: RequestId,
        candidate_id: CandidateId,
        decision_id: impl Into<String>,
        expected: BranchHead,
    ) -> Result<PublishReceipt> {
        self.publish_guarded(
            request,
            candidate_id,
            decision_id.into(),
            expected,
            None,
            "publish",
        )
    }

    pub fn publish_merge(
        &self,
        request: RequestId,
        candidate_id: CandidateId,
        decision_id: impl Into<String>,
        intent: MergeIntent,
    ) -> Result<PublishReceipt> {
        if intent.candidate_id != candidate_id {
            return Err(CensorFsError::bad_request(
                "merge intent does not belong to candidate",
            ));
        }
        if intent.source_branch == intent.target_branch {
            return Err(CensorFsError::bad_request(
                "merge source and target branches must differ",
            ));
        }
        let candidate = self.candidate(candidate_id)?;
        if candidate.branch_id != intent.target_branch
            || candidate.base_generation != intent.target_head.generation_id
        {
            return Err(CensorFsError::bad_request(
                "merge intent does not match candidate target",
            ));
        }
        let generation = self.generation(candidate.result_generation)?;
        if generation.kind != GenerationKind::Merge
            || generation.parents
                != vec![
                    intent.target_head.generation_id,
                    intent.source_head.generation_id,
                ]
        {
            return Err(CensorFsError::corrupt(
                "merge generation parents do not match merge intent",
            ));
        }
        self.publish_guarded(
            request,
            candidate_id,
            decision_id.into(),
            intent.target_head,
            Some((intent.source_branch, intent.source_head)),
            "publish_merge",
        )
    }

    fn publish_guarded(
        &self,
        request: RequestId,
        candidate_id: CandidateId,
        decision_id: String,
        expected: BranchHead,
        source_guard: Option<(BranchId, BranchHead)>,
        operation: &'static str,
    ) -> Result<PublishReceipt> {
        self.ensure_writable()?;
        self.idempotent(request, operation, || {
            if let Some(receipt) = self.inner.state.read().receipts.get(&candidate_id).cloned() {
                return Ok(receipt);
            }
            let candidate = self
                .inner
                .state
                .read()
                .candidates
                .get(&candidate_id)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("candidate", candidate_id))?;
            let ticket_lock = candidate.ticket_id.map(|ticket| self.ticket_lock(ticket));
            let _ticket_guard = ticket_lock.as_ref().map(|lock| lock.lock());
            let candidate = self
                .inner
                .state
                .read()
                .candidates
                .get(&candidate_id)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("candidate", candidate_id))?;
            if candidate.state != CandidateState::Prepared {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "candidate is not prepared",
                ));
            }
            if expected.generation_id != candidate.base_generation
                || expected.head_seq != candidate.expected_head_seq
            {
                return Err(CensorFsError::bad_request(
                    "publish expected head does not match candidate",
                ));
            }
            let target_lock = self.branch_lock(&candidate.branch_id);
            let source_lock = source_guard
                .as_ref()
                .map(|(source, _)| self.branch_lock(source));
            let target_first = source_guard
                .as_ref()
                .is_none_or(|(source, _)| candidate.branch_id <= *source);
            let (_first_guard, _second_guard) = if target_first {
                let first = target_lock.lock();
                let second = source_lock.as_ref().map(|lock| lock.lock());
                (first, second)
            } else {
                let first = source_lock
                    .as_ref()
                    .expect("source lock exists when source sorts first")
                    .lock();
                let second = Some(target_lock.lock());
                (first, second)
            };
            let candidate = self
                .inner
                .state
                .read()
                .candidates
                .get(&candidate_id)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("candidate", candidate_id))?;
            if candidate.state != CandidateState::Prepared {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "candidate is not prepared",
                ));
            }
            if let Some((source_branch, source_expected)) = &source_guard {
                let current_source = self.branch_head(source_branch)?;
                if &current_source != source_expected {
                    return Err(CensorFsError::new(
                        ErrorCode::HeadChanged,
                        format!(
                            "source head changed from {}/{} to {}/{}",
                            source_expected.generation_id,
                            source_expected.head_seq,
                            current_source.generation_id,
                            current_source.head_seq
                        ),
                    ));
                }
            }
            let current = self.branch_head(&candidate.branch_id)?;
            if current != expected {
                return Err(CensorFsError::new(
                    ErrorCode::HeadChanged,
                    format!(
                        "current head is {}/{}",
                        current.generation_id, current.head_seq
                    ),
                ));
            }
            self.inner
                .store
                .load_generation(candidate.result_generation)?;
            self.journal(
                request,
                JournalKind::HeadSwitchPrepare,
                candidate_id.to_string(),
                &candidate,
            )?;
            let next = BranchHead {
                generation_id: candidate.result_generation,
                head_seq: current
                    .head_seq
                    .checked_add(1)
                    .ok_or_else(|| CensorFsError::corrupt("branch head sequence overflow"))?,
            };
            if let Err(error) =
                write_next_head(&self.inner.persist, &candidate.branch_id, &current, &next)
            {
                if read_best_head(&self.inner.persist, &candidate.branch_id)
                    .ok()
                    .is_some_and(|(head, _)| head == next)
                {
                    self.inner
                        .state
                        .write()
                        .branches
                        .insert(candidate.branch_id.clone(), next);
                    return Err(CensorFsError::new(
                        ErrorCode::ResultUnknown,
                        format!("head may have switched at a failed durability boundary: {error}"),
                    ));
                }
                return Err(error);
            }
            self.inner.persist.fault().checkpoint("head_slot.durable")?;
            self.inner
                .state
                .write()
                .branches
                .insert(candidate.branch_id.clone(), next.clone());
            self.journal(
                request,
                JournalKind::HeadSwitchCommit,
                candidate_id.to_string(),
                &next,
            )?;
            let receipt = PublishReceipt {
                branch_id: candidate.branch_id.clone(),
                head_seq: next.head_seq,
                old_generation: current.generation_id,
                new_generation: next.generation_id,
                candidate_id,
                ticket_id: candidate.ticket_id,
                tx_id: candidate.tx_id,
                decision_id,
                request_id: request,
            };
            let receipt_path = self
                .inner
                .persist
                .receipt_path(&candidate.branch_id, next.head_seq);
            if let Err(error) =
                self.inner
                    .persist
                    .write_record(&receipt_path, RecordKind::Receipt, &receipt, true)
            {
                return Err(CensorFsError::new(
                    ErrorCode::ResultUnknown,
                    format!("head switched but receipt write failed: {error}"),
                ));
            }
            self.inner.persist.fault().checkpoint("receipt.durable")?;
            let mut updated_candidate = candidate.clone();
            updated_candidate.state = CandidateState::Published;
            self.inner.persist.write_record(
                &self.inner.persist.candidate_path(candidate_id),
                RecordKind::Candidate,
                &updated_candidate,
                false,
            )?;
            let mut state = self.inner.state.write();
            state.candidates.insert(candidate_id, updated_candidate);
            if let Some(ticket_id) = candidate.ticket_id {
                if let Some(ticket) = state.ticket_meta.get_mut(&ticket_id) {
                    ticket.state = TicketState::Published;
                    self.inner.persist.write_record(
                        &self.inner.persist.ticket_meta_path(ticket_id),
                        RecordKind::Ticket,
                        ticket,
                        false,
                    )?;
                }
                state
                    .publication_index
                    .insert((candidate.tx_id, ticket_id), next.generation_id);
            }
            state.receipts.insert(candidate_id, receipt.clone());
            Ok(receipt)
        })
    }

    fn branch_lock(&self, branch: &BranchId) -> Arc<Mutex<()>> {
        self.inner
            .branch_locks
            .lock()
            .entry(branch.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn request_lock(&self, request: RequestId) -> Arc<Mutex<()>> {
        self.inner
            .request_locks
            .lock()
            .entry(request)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn ticket_lock(&self, ticket: TicketId) -> Arc<Mutex<()>> {
        self.inner
            .ticket_locks
            .lock()
            .entry(ticket)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn tx_lock(&self, tx: TxId) -> Arc<Mutex<()>> {
        self.inner
            .tx_locks
            .lock()
            .entry(tx)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub fn abort_ticket(&self, request: RequestId, ticket: TicketId) -> Result<TicketMeta> {
        self.ensure_writable()?;
        self.idempotent(request, "abort_ticket", || {
            self.abort_ticket_inner(request, ticket)
        })
    }

    fn abort_ticket_inner(&self, request: RequestId, ticket_id: TicketId) -> Result<TicketMeta> {
        let ticket_lock = self.ticket_lock(ticket_id);
        let _ticket_guard = ticket_lock.lock();
        let mut meta = self
            .inner
            .state
            .read()
            .ticket_meta
            .get(&ticket_id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("ticket", ticket_id))?;
        if meta.state == TicketState::Published {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                "published ticket cannot be aborted",
            ));
        }
        if meta.state == TicketState::Aborted {
            return Ok(meta);
        }
        let branch_lock = self.branch_lock(&meta.branch_id);
        let _branch_guard = branch_lock.lock();
        meta = self
            .inner
            .state
            .read()
            .ticket_meta
            .get(&ticket_id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("ticket", ticket_id))?;
        if meta.state == TicketState::Published {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                "published ticket cannot be aborted",
            ));
        }
        if meta.state == TicketState::Aborted {
            return Ok(meta);
        }
        meta.state = TicketState::Aborted;
        meta.terminal_request = Some(request);
        if let Some(runtime) = self.inner.state.read().tickets.get(&ticket_id).cloned() {
            *runtime.meta.write() = meta.clone();
        }
        self.inner.persist.write_record(
            &self.inner.persist.ticket_meta_path(ticket_id),
            RecordKind::Ticket,
            &meta,
            false,
        )?;
        if let Some(candidate_id) = meta.candidate_id {
            let candidate = {
                self.inner
                    .state
                    .read()
                    .candidates
                    .get(&candidate_id)
                    .cloned()
            };
            if let Some(mut candidate) = candidate {
                candidate.state = CandidateState::Aborted;
                self.inner.persist.write_record(
                    &self.inner.persist.candidate_path(candidate_id),
                    RecordKind::Candidate,
                    &candidate,
                    false,
                )?;
                self.inner
                    .state
                    .write()
                    .candidates
                    .insert(candidate_id, candidate);
            }
        }
        self.inner.views.revoke_ticket_views(ticket_id);
        self.journal(
            request,
            JournalKind::TicketAborted,
            ticket_id.to_string(),
            &meta,
        )?;
        self.inner
            .state
            .write()
            .ticket_meta
            .insert(ticket_id, meta.clone());
        Ok(meta)
    }

    pub fn generation(&self, id: GenerationId) -> Result<GenerationMeta> {
        self.inner.store.load_generation(id).map(|v| v.0)
    }
    pub fn manifest(&self, id: GenerationId) -> Result<Manifest> {
        self.inner.store.load_generation(id).map(|value| value.1)
    }
    pub fn diff(&self, left: GenerationId, right: GenerationId) -> Result<Vec<PathDiff>> {
        self.inner.store.diff(left, right)
    }
    pub fn text_diff(
        &self,
        left: GenerationId,
        right: GenerationId,
        max_file_bytes: usize,
    ) -> Result<TextDiffReport> {
        self.inner.store.text_diff(left, right, max_file_bytes)
    }

    pub fn resolve_fork_point(&self, tx: TxId, ticket: TicketId) -> Result<GenerationId> {
        self.inner
            .state
            .read()
            .publication_index
            .get(&(tx, ticket))
            .copied()
            .ok_or_else(|| {
                CensorFsError::new(
                    ErrorCode::NotFound,
                    "tx/ticket does not identify a published generation",
                )
            })
    }

    pub fn request_result(&self, request: RequestId) -> Result<Option<PersistedResponse>> {
        let path = self.inner.persist.request_path(request);
        if !path.exists() {
            return Ok(None);
        }
        self.inner
            .persist
            .read_record(&path, RecordKind::RequestResult)
            .map(Some)
    }

    pub fn ticket(&self, id: TicketId) -> Result<TicketMeta> {
        self.inner
            .state
            .read()
            .ticket_meta
            .get(&id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("ticket", id))
    }

    pub fn tx(&self, id: TxId) -> Result<TxMeta> {
        self.inner
            .state
            .read()
            .txs
            .get(&id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("transaction", id))
    }

    pub fn candidate(&self, id: CandidateId) -> Result<CandidateMeta> {
        self.inner
            .state
            .read()
            .candidates
            .get(&id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("candidate", id))
    }

    fn rebuild_stable_request_results(&self) -> Result<()> {
        let state = self.inner.state.read();
        for tx in state.txs.values() {
            if !self
                .inner
                .persist
                .request_path(tx.created_by_request)
                .exists()
            {
                let mut original = tx.clone();
                original.state = TxState::Open;
                original.tickets.clear();
                original.terminal_request = None;
                self.inner
                    .persist
                    .save_response(tx.created_by_request, "begin_tx", &original)?;
            }
            if let Some(request) = tx.terminal_request {
                let operation = if tx.state == TxState::Aborted {
                    "abort_tx"
                } else {
                    "close_tx"
                };
                if !self.inner.persist.request_path(request).exists() {
                    self.inner.persist.save_response(request, operation, tx)?;
                }
            }
        }
        for meta in state.branch_meta.values() {
            if let Some(request) = meta.created_by_request {
                if !self.inner.persist.request_path(request).exists() {
                    let original = BranchHead {
                        generation_id: meta.created_from,
                        head_seq: 0,
                    };
                    self.inner
                        .persist
                        .save_response(request, "create_branch", &original)?;
                }
            }
        }
        for ticket in state.ticket_meta.values() {
            if !self
                .inner
                .persist
                .request_path(ticket.created_by_request)
                .exists()
            {
                let original = original_ticket_result(ticket);
                self.inner.persist.save_response(
                    ticket.created_by_request,
                    "begin_ticket",
                    &original,
                )?;
            }
            if let Some(request) = ticket.terminal_request {
                if !self.inner.persist.request_path(request).exists() {
                    self.inner
                        .persist
                        .save_response(request, "abort_ticket", ticket)?;
                }
            }
        }
        for candidate in state.candidates.values() {
            if !self
                .inner
                .persist
                .request_path(candidate.created_by_request)
                .exists()
            {
                let (generation, _) = self
                    .inner
                    .store
                    .load_generation(candidate.result_generation)?;
                let mut original_candidate = candidate.clone();
                original_candidate.state = CandidateState::Prepared;
                if generation.kind == GenerationKind::Merge {
                    let intent: MergeIntent = self.inner.persist.read_record(
                        &self.inner.persist.merge_intent_path(candidate.candidate_id),
                        RecordKind::MergeIntent,
                    )?;
                    let result = MergePrepareResult {
                        check: MergeCheckResult {
                            source_branch: intent.source_branch.clone(),
                            target_branch: intent.target_branch.clone(),
                            source_head: intent.source_head.clone(),
                            target_head: intent.target_head.clone(),
                            merge_base: intent.merge_base,
                            conflicts: Vec::new(),
                            already_up_to_date: false,
                        },
                        intent,
                        candidate: original_candidate,
                        generation,
                    };
                    self.inner.persist.save_response(
                        candidate.created_by_request,
                        "build_merge_candidate",
                        &result,
                    )?;
                } else {
                    let result = PrepareResult {
                        candidate: original_candidate,
                        generation,
                    };
                    let operation = if candidate.rollback_target.is_some() {
                        "build_rollback_candidate"
                    } else {
                        "prepare_ticket"
                    };
                    self.inner.persist.save_response(
                        candidate.created_by_request,
                        operation,
                        &result,
                    )?;
                }
            }
        }
        for receipt in state.receipts.values() {
            if !self.inner.persist.request_path(receipt.request_id).exists() {
                let operation = if self
                    .inner
                    .persist
                    .merge_intent_path(receipt.candidate_id)
                    .exists()
                {
                    "publish_merge"
                } else {
                    "publish"
                };
                self.inner
                    .persist
                    .save_response(receipt.request_id, operation, receipt)?;
            }
        }
        Ok(())
    }
}

fn original_ticket_result(ticket: &TicketMeta) -> TicketMeta {
    let mut original = ticket.clone();
    original.state = TicketState::Open;
    original.candidate_id = None;
    original.terminal_request = None;
    original
}

pub(crate) fn read_superblock(persist: &Persistence) -> Result<(Superblock, char)> {
    let mut valid = Vec::new();
    for slot in ['a', 'b'] {
        let path = persist.path(format!("superblock/slot.{slot}"));
        if path.exists() {
            if let Ok(value) = persist.read_record::<Superblock>(&path, RecordKind::Superblock) {
                valid.push((value, slot));
            }
        }
    }
    if valid.len() == 2 && valid[0].0.mount_seq == valid[1].0.mount_seq && valid[0].0 != valid[1].0
    {
        return Err(CensorFsError::corrupt(
            "superblock slots have equal sequence with different values",
        ));
    }
    valid.sort_by_key(|(value, _)| value.mount_seq);
    valid
        .pop()
        .ok_or_else(|| CensorFsError::corrupt("no valid superblock slot"))
}

pub(crate) fn read_best_head(
    persist: &Persistence,
    branch: &BranchId,
) -> Result<(BranchHead, char)> {
    let dir = persist.branch_dir(branch);
    let mut valid = Vec::new();
    for slot in ['a', 'b'] {
        let path = dir.join(format!("head.{slot}"));
        if path.exists() {
            if let Ok(value) = persist.read_record::<BranchHead>(&path, RecordKind::HeadSlot) {
                valid.push((value, slot));
            }
        }
    }
    if valid.len() == 2
        && valid[0].0.head_seq == valid[1].0.head_seq
        && valid[0].0.generation_id != valid[1].0.generation_id
    {
        return Err(CensorFsError::corrupt(
            "head slots have equal sequence with different generations",
        ));
    }
    valid.sort_by_key(|(value, _)| value.head_seq);
    valid
        .pop()
        .ok_or_else(|| CensorFsError::corrupt("no valid head slot"))
}

fn write_next_head(
    persist: &Persistence,
    branch: &BranchId,
    expected: &BranchHead,
    next: &BranchHead,
) -> Result<()> {
    let (current, active) = read_best_head(persist, branch)?;
    if &current != expected {
        return Err(CensorFsError::new(
            ErrorCode::HeadChanged,
            "head changed while publishing",
        ));
    }
    if next.head_seq != expected.head_seq + 1 {
        return Err(CensorFsError::bad_request(
            "next head sequence is not monotonic",
        ));
    }
    let target = if active == 'a' { 'b' } else { 'a' };
    persist.write_record(
        &persist.branch_dir(branch).join(format!("head.{target}")),
        RecordKind::HeadSlot,
        next,
        false,
    )
}

fn path_to_logical(path: &Path) -> Result<LogicalPath> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        LogicalPath::new(path.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        LogicalPath::from_utf8(&path.to_string_lossy().replace('\\', "/"))
    }
}
