use crate::branch::{read_best_head, read_superblock};
use crate::codec::{self, RecordKind};
use crate::error::{Result, CensorFsError};
use crate::fault::FaultInjector;
use crate::ids::*;
use crate::model::*;
use crate::persist::Persistence;
use crate::store::VersionStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FsckStats {
    pub branches: usize,
    pub generations: usize,
    pub manifests: usize,
    pub objects: usize,
    pub transactions: usize,
    pub tickets: usize,
    pub candidates: usize,
    pub receipts: usize,
    pub requests: usize,
    pub journal_records: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsckIssue {
    pub code: String,
    pub path: String,
    pub message: String,
    pub repairable: bool,
    pub read_only_reason: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsckReport {
    pub clean: bool,
    pub repair_requested: bool,
    pub repairs: Vec<String>,
    pub issues: Vec<FsckIssue>,
    pub read_only_reasons: Vec<String>,
    pub stats: FsckStats,
}

impl FsckReport {
    fn new(repair_requested: bool) -> Self {
        Self {
            clean: true,
            repair_requested,
            repairs: Vec::new(),
            issues: Vec::new(),
            read_only_reasons: Vec::new(),
            stats: FsckStats::default(),
        }
    }

    fn issue(
        &mut self,
        code: &str,
        path: impl AsRef<Path>,
        message: impl Into<String>,
        repairable: bool,
        read_only_reason: bool,
    ) {
        let message = message.into();
        if read_only_reason {
            self.read_only_reasons.push(message.clone());
        }
        self.issues.push(FsckIssue {
            code: code.into(),
            path: path.as_ref().display().to_string(),
            message,
            repairable,
            read_only_reason,
        });
        self.clean = false;
    }
}

pub fn run_fsck(storage_root: impl Into<PathBuf>, repair: bool) -> Result<FsckReport> {
    let storage_root = storage_root.into();
    if !storage_root.is_dir() {
        return Err(CensorFsError::bad_request(format!(
            "storage root {} is not an initialized instance",
            storage_root.display()
        )));
    }
    let persist = Persistence::new(storage_root, Arc::new(FaultInjector::disabled()));
    let _lock = persist.acquire_existing_lock()?;
    if !repair {
        return scan(&persist, false);
    }

    let repaired = scan(&persist, true)?;
    let mut verified = scan(&persist, false)?;
    verified.repair_requested = true;
    verified.repairs = repaired.repairs;
    Ok(verified)
}

fn scan(persist: &Persistence, repair: bool) -> Result<FsckReport> {
    let mut report = FsckReport::new(repair);
    let store = VersionStore::new(persist.clone());
    let (journal, valid_end, actual_end) = persist.inspect_journal()?;
    report.stats.journal_records = journal.len();
    if valid_end != actual_end {
        if repair {
            persist.recover_journal()?;
            report.repairs.push(format!(
                "truncated journal tail from {actual_end} to {valid_end} bytes"
            ));
        } else {
            report.issue(
                "journal_tail",
                persist.path("journal/current.log"),
                format!("journal has an invalid tail after byte {valid_end}"),
                true,
                false,
            );
        }
    }

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

    let (superblock, _) = match read_superblock(persist) {
        Ok(value) => value,
        Err(error) => {
            report.issue(
                "superblock",
                persist.path("superblock"),
                error.to_string(),
                false,
                true,
            );
            return Ok(report);
        }
    };
    repair_bad_slot(
        persist,
        RecordKind::Superblock,
        &superblock,
        persist.path("superblock/slot.a"),
        repair,
        &mut report,
    )?;
    repair_bad_slot(
        persist,
        RecordKind::Superblock,
        &superblock,
        persist.path("superblock/slot.b"),
        repair,
        &mut report,
    )?;

    let mut generations = HashMap::new();
    for path in records_in(persist.path("generations"), "meta")? {
        report.stats.generations += 1;
        let Some(id) = parse_stem(&path, GenerationId::parse) else {
            report.issue(
                "generation_name",
                &path,
                "invalid generation file name",
                false,
                true,
            );
            continue;
        };
        match store.load_generation(id) {
            Ok((meta, _)) => {
                generations.insert(id, meta);
            }
            Err(error) => report.issue("generation", &path, error.to_string(), false, true),
        }
    }
    for (id, meta) in &generations {
        for parent in &meta.parents {
            if !generations.contains_key(parent) {
                report.issue(
                    "generation_parent",
                    persist.generation_path(*id),
                    format!("generation {id} references missing parent {parent}"),
                    false,
                    true,
                );
            }
        }
    }

    for path in records_in(persist.path("manifests"), "manifest")? {
        report.stats.manifests += 1;
        match persist.read_record::<Manifest>(&path, RecordKind::Manifest) {
            Ok(manifest) => {
                if let Err(error) = VersionStore::validate_manifest(&manifest) {
                    report.issue("manifest", &path, error.to_string(), false, true);
                }
            }
            Err(error) => report.issue("manifest", &path, error.to_string(), false, true),
        }
    }
    for path in records_in(persist.path("objects"), "data")? {
        report.stats.objects += 1;
        let Some(id) = parse_stem(&path, ObjectId::parse) else {
            report.issue(
                "object_name",
                &path,
                "invalid object file name",
                false,
                true,
            );
            continue;
        };
        if let Err(error) = store.validate_object(id) {
            report.issue("object", &path, error.to_string(), false, true);
        }
    }

    let mut branches = HashMap::new();
    for entry in fs::read_dir(persist.path("branches"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let meta_path = entry.path().join("branch.meta");
        let meta: BranchMeta = match persist.read_record(&meta_path, RecordKind::BranchMeta) {
            Ok(value) => value,
            Err(error) => {
                report.issue("branch_meta", &meta_path, error.to_string(), false, true);
                continue;
            }
        };
        report.stats.branches += 1;
        let (head, _) = match read_best_head(persist, &meta.branch_id) {
            Ok(value) => value,
            Err(error) => {
                report.issue("branch_head", entry.path(), error.to_string(), false, true);
                continue;
            }
        };
        for slot in ['a', 'b'] {
            repair_bad_slot(
                persist,
                RecordKind::HeadSlot,
                &head,
                entry.path().join(format!("head.{slot}")),
                repair,
                &mut report,
            )?;
        }
        if !generations.contains_key(&head.generation_id) {
            report.issue(
                "branch_head_generation",
                entry.path(),
                format!(
                    "branch {} head references invalid generation {}",
                    meta.branch_id, head.generation_id
                ),
                false,
                true,
            );
        }
        branches.insert(meta.branch_id.clone(), (meta, head));
    }

    let mut txs = HashMap::new();
    for path in records_in(persist.path("txs"), "meta")? {
        report.stats.transactions += 1;
        match persist.read_record::<TxMeta>(&path, RecordKind::Tx) {
            Ok(meta) => {
                txs.insert(meta.tx_id, meta);
            }
            Err(error) => report.issue("transaction", &path, error.to_string(), false, true),
        }
    }

    let mut candidates = HashMap::new();
    for path in records_in(persist.path("candidates"), "meta")? {
        report.stats.candidates += 1;
        match persist.read_record::<CandidateMeta>(&path, RecordKind::Candidate) {
            Ok(meta) => {
                if !txs.contains_key(&meta.tx_id) {
                    report.issue(
                        "candidate_tx",
                        &path,
                        format!(
                            "candidate {} references missing tx {}",
                            meta.candidate_id, meta.tx_id
                        ),
                        false,
                        true,
                    );
                }
                if !branches.contains_key(&meta.branch_id) {
                    report.issue(
                        "candidate_branch",
                        &path,
                        format!("candidate {} references missing branch", meta.candidate_id),
                        false,
                        true,
                    );
                }
                if !generations.contains_key(&meta.result_generation) {
                    report.issue(
                        "candidate_generation",
                        &path,
                        format!(
                            "candidate {} references invalid generation",
                            meta.candidate_id
                        ),
                        false,
                        true,
                    );
                }
                candidates.insert(meta.candidate_id, meta);
            }
            Err(error) => report.issue("candidate", &path, error.to_string(), false, true),
        }
    }
    let candidate_by_ticket: HashMap<TicketId, CandidateId> = candidates
        .values()
        .filter_map(|candidate| {
            candidate
                .ticket_id
                .map(|ticket| (ticket, candidate.candidate_id))
        })
        .collect();

    let mut tickets = HashMap::new();
    for entry in fs::read_dir(persist.path("tickets"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("ticket.meta");
        if !path.exists() {
            continue;
        }
        report.stats.tickets += 1;
        let mut meta: TicketMeta = match persist.read_record(&path, RecordKind::Ticket) {
            Ok(value) => value,
            Err(error) => {
                report.issue("ticket", &path, error.to_string(), false, true);
                continue;
            }
        };
        if !txs.contains_key(&meta.tx_id) || !branches.contains_key(&meta.branch_id) {
            report.issue(
                "ticket_reference",
                &path,
                format!(
                    "ticket {} references a missing tx or branch",
                    meta.ticket_id
                ),
                false,
                true,
            );
        }
        let inferred = meta
            .candidate_id
            .or_else(|| candidate_by_ticket.get(&meta.ticket_id).copied());
        if matches!(meta.state, TicketState::Open | TicketState::Freezing) {
            if let Some(candidate_id) = inferred {
                if repair {
                    meta.candidate_id = Some(candidate_id);
                    meta.state = TicketState::Prepared;
                    persist.write_record(&path, RecordKind::Ticket, &meta, false)?;
                    report.repairs.push(format!(
                        "restored candidate association for ticket {}",
                        meta.ticket_id
                    ));
                } else {
                    report.issue(
                        "ticket_candidate_association",
                        &path,
                        format!("ticket {} can be restored to PREPARED", meta.ticket_id),
                        true,
                        false,
                    );
                }
            } else if repair {
                meta.state = TicketState::Aborted;
                let terminal = meta.created_by_request.derive("fsck_abort_ticket");
                meta.terminal_request = Some(terminal);
                persist.write_record(&path, RecordKind::Ticket, &meta, false)?;
                append_repair_journal(persist, terminal, &meta)?;
                report
                    .repairs
                    .push(format!("aborted open ticket {}", meta.ticket_id));
            } else {
                report.issue(
                    "open_ticket",
                    &path,
                    format!("ticket {} must be aborted during recovery", meta.ticket_id),
                    true,
                    false,
                );
            }
        }
        if meta.state == TicketState::Prepared
            && meta
                .candidate_id
                .is_none_or(|candidate| !candidates.contains_key(&candidate))
        {
            report.issue(
                "prepared_ticket",
                &path,
                format!(
                    "ticket {} is PREPARED without a valid candidate",
                    meta.ticket_id
                ),
                false,
                true,
            );
        }
        tickets.insert(meta.ticket_id, meta);
    }

    let mut receipts = HashMap::new();
    for branch_dir in fs::read_dir(persist.path("publish"))? {
        let branch_dir = branch_dir?;
        if !branch_dir.file_type()?.is_dir() {
            continue;
        }
        for path in records_in(branch_dir.path(), "receipt")? {
            report.stats.receipts += 1;
            match persist.read_record::<PublishReceipt>(&path, RecordKind::Receipt) {
                Ok(receipt) => {
                    match candidates.get(&receipt.candidate_id) {
                        Some(candidate)
                            if candidate.result_generation == receipt.new_generation
                                && candidate.branch_id == receipt.branch_id => {}
                        _ => report.issue(
                            "receipt_candidate",
                            &path,
                            format!(
                                "receipt for candidate {} is inconsistent",
                                receipt.candidate_id
                            ),
                            false,
                            true,
                        ),
                    }
                    receipts.insert(receipt.candidate_id, receipt);
                }
                Err(error) => report.issue("receipt", &path, error.to_string(), false, true),
            }
        }
    }

    for (branch, (_, head)) in &branches {
        let Some(candidate) = candidates
            .values()
            .find(|candidate| {
                candidate.branch_id == *branch && candidate.result_generation == head.generation_id
            })
            .cloned()
        else {
            continue;
        };
        if receipts.contains_key(&candidate.candidate_id) {
            continue;
        }
        if repair {
            let request_id = publish_requests
                .get(&candidate.candidate_id)
                .copied()
                .unwrap_or_else(|| candidate.created_by_request.derive("fsck_receipt"));
            let receipt = PublishReceipt {
                branch_id: branch.clone(),
                head_seq: head.head_seq,
                old_generation: candidate.base_generation,
                new_generation: candidate.result_generation,
                candidate_id: candidate.candidate_id,
                ticket_id: candidate.ticket_id,
                tx_id: candidate.tx_id,
                decision_id: "recovered-by-fsck".into(),
                request_id,
            };
            persist.write_record(
                &persist.receipt_path(branch, head.head_seq),
                RecordKind::Receipt,
                &receipt,
                true,
            )?;
            let mut updated = candidate.clone();
            updated.state = CandidateState::Published;
            persist.write_record(
                &persist.candidate_path(updated.candidate_id),
                RecordKind::Candidate,
                &updated,
                false,
            )?;
            candidates.insert(updated.candidate_id, updated);
            if let Some(ticket_id) = candidate.ticket_id {
                if let Some(ticket) = tickets.get_mut(&ticket_id) {
                    ticket.state = TicketState::Published;
                    persist.write_record(
                        &persist.ticket_meta_path(ticket_id),
                        RecordKind::Ticket,
                        ticket,
                        false,
                    )?;
                }
            }
            receipts.insert(candidate.candidate_id, receipt);
            report.repairs.push(format!(
                "recreated receipt for candidate {}",
                candidate.candidate_id
            ));
        } else {
            report.issue(
                "missing_receipt",
                persist.receipt_path(branch, head.head_seq),
                format!(
                    "published head is missing receipt for candidate {}",
                    candidate.candidate_id
                ),
                true,
                false,
            );
        }
    }

    validate_merge_intents(persist, &generations, &candidates, &mut report)?;
    validate_and_rebuild_requests(
        persist,
        &branches,
        &txs,
        &tickets,
        &candidates,
        &receipts,
        repair,
        &mut report,
    )?;
    report.clean = report.issues.is_empty();
    Ok(report)
}

fn repair_bad_slot<T>(
    persist: &Persistence,
    kind: RecordKind,
    selected: &T,
    path: PathBuf,
    repair: bool,
    report: &mut FsckReport,
) -> Result<()>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    if !path.exists() || persist.read_record::<T>(&path, kind).is_ok() {
        return Ok(());
    }
    if repair {
        persist.write_record(&path, kind, selected, false)?;
        report
            .repairs
            .push(format!("restored invalid A/B slot {}", path.display()));
    } else {
        report.issue(
            "invalid_slot",
            path,
            "slot is invalid; the other valid slot was selected",
            true,
            false,
        );
    }
    Ok(())
}

fn append_repair_journal(
    persist: &Persistence,
    request: RequestId,
    ticket: &TicketMeta,
) -> Result<()> {
    let payload_digest = *blake3::hash(&codec::serialize(ticket)?).as_bytes();
    persist.append_journal(JournalRecord {
        seq: 0,
        operation_id: request,
        kind: JournalKind::TicketAborted,
        target_id: ticket.ticket_id.to_string(),
        payload_digest,
    })?;
    Ok(())
}

fn validate_merge_intents(
    persist: &Persistence,
    generations: &HashMap<GenerationId, GenerationMeta>,
    candidates: &HashMap<CandidateId, CandidateMeta>,
    report: &mut FsckReport,
) -> Result<()> {
    for candidate in candidates.values() {
        let is_merge = generations
            .get(&candidate.result_generation)
            .is_some_and(|generation| generation.kind == GenerationKind::Merge);
        if !is_merge {
            continue;
        }
        let path = persist.merge_intent_path(candidate.candidate_id);
        match persist.read_record::<MergeIntent>(&path, RecordKind::MergeIntent) {
            Ok(intent)
                if intent.candidate_id == candidate.candidate_id
                    && intent.target_branch == candidate.branch_id => {}
            Ok(_) => report.issue(
                "merge_intent",
                path,
                "merge intent does not match its candidate",
                false,
                true,
            ),
            Err(error) => report.issue("merge_intent", path, error.to_string(), false, true),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_and_rebuild_requests(
    persist: &Persistence,
    branches: &HashMap<BranchId, (BranchMeta, BranchHead)>,
    txs: &HashMap<TxId, TxMeta>,
    tickets: &HashMap<TicketId, TicketMeta>,
    candidates: &HashMap<CandidateId, CandidateMeta>,
    receipts: &HashMap<CandidateId, PublishReceipt>,
    repair: bool,
    report: &mut FsckReport,
) -> Result<()> {
    for path in records_in(persist.path("requests"), "result")? {
        report.stats.requests += 1;
        if let Err(error) =
            persist.read_record::<PersistedResponse>(&path, RecordKind::RequestResult)
        {
            report.issue("request_result", path, error.to_string(), false, true);
        }
    }
    for tx in txs.values() {
        let mut initial = tx.clone();
        initial.state = TxState::Open;
        initial.tickets.clear();
        initial.terminal_request = None;
        ensure_response(
            persist,
            tx.created_by_request,
            "begin_tx",
            &initial,
            repair,
            report,
        )?;
        if let Some(request) = tx.terminal_request {
            let operation = if tx.state == TxState::Aborted {
                "abort_tx"
            } else {
                "close_tx"
            };
            ensure_response(persist, request, operation, tx, repair, report)?;
        }
    }
    for (meta, _) in branches.values() {
        if let Some(request) = meta.created_by_request {
            ensure_response(
                persist,
                request,
                "create_branch",
                &BranchHead {
                    generation_id: meta.created_from,
                    head_seq: 0,
                },
                repair,
                report,
            )?;
        }
    }
    for ticket in tickets.values() {
        let mut initial = ticket.clone();
        initial.state = TicketState::Open;
        initial.candidate_id = None;
        initial.terminal_request = None;
        ensure_response(
            persist,
            ticket.created_by_request,
            "begin_ticket",
            &initial,
            repair,
            report,
        )?;
        if ticket.state == TicketState::Aborted {
            if let Some(request) = ticket.terminal_request {
                ensure_response(persist, request, "abort_ticket", ticket, repair, report)?;
            }
        }
    }
    let store = VersionStore::new(persist.clone());
    for candidate in candidates.values() {
        let Some(generation) = store.load_generation_meta(candidate.result_generation).ok() else {
            continue;
        };
        if generation.kind == GenerationKind::Merge {
            let intent: MergeIntent = match persist.read_record(
                &persist.merge_intent_path(candidate.candidate_id),
                RecordKind::MergeIntent,
            ) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let mut initial_candidate = candidate.clone();
            initial_candidate.state = CandidateState::Prepared;
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
                candidate: initial_candidate,
                generation,
            };
            ensure_response(
                persist,
                candidate.created_by_request,
                "build_merge_candidate",
                &result,
                repair,
                report,
            )?;
        } else {
            let mut initial_candidate = candidate.clone();
            initial_candidate.state = CandidateState::Prepared;
            let result = PrepareResult {
                candidate: initial_candidate,
                generation,
            };
            let operation = if candidate.rollback_target.is_some() {
                "build_rollback_candidate"
            } else {
                "prepare_ticket"
            };
            ensure_response(
                persist,
                candidate.created_by_request,
                operation,
                &result,
                repair,
                report,
            )?;
        }
    }
    for receipt in receipts.values() {
        let operation = if persist.merge_intent_path(receipt.candidate_id).exists() {
            "publish_merge"
        } else {
            "publish"
        };
        ensure_response(
            persist,
            receipt.request_id,
            operation,
            receipt,
            repair,
            report,
        )?;
    }
    Ok(())
}

fn ensure_response<T: Serialize>(
    persist: &Persistence,
    request: RequestId,
    operation: &str,
    value: &T,
    repair: bool,
    report: &mut FsckReport,
) -> Result<()> {
    let path = persist.request_path(request);
    if path.exists() {
        match persist.read_record::<PersistedResponse>(&path, RecordKind::RequestResult) {
            Ok(response) if response.operation == operation => return Ok(()),
            Ok(response) => {
                report.issue(
                    "request_operation",
                    path,
                    format!(
                        "request result operation is {}, expected {operation}",
                        response.operation
                    ),
                    false,
                    true,
                );
                return Ok(());
            }
            Err(_) => return Ok(()),
        }
    }
    if repair {
        persist.save_response(request, operation, value)?;
        report
            .repairs
            .push(format!("rebuilt request result {request} ({operation})"));
    } else {
        report.issue(
            "missing_request_result",
            path,
            format!("request result for {operation} can be rebuilt"),
            true,
            false,
        );
    }
    Ok(())
}

fn records_in(directory: PathBuf, extension: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn parse_stem<T>(
    path: &Path,
    parse: impl FnOnce(&str) -> std::result::Result<T, uuid::Error>,
) -> Option<T> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| parse(value).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InitOptions, CensorFs};

    #[test]
    fn clean_instance_and_open_ticket_repair() {
        std::env::set_var("CENSORFS_ALLOW_UNSUPPORTED_BACKING", "1");
        let root = std::env::temp_dir().join(format!("censorfs-fsck-{}", uuid::Uuid::new_v4()));
        let import = root.join("import");
        let storage = root.join("store");
        fs::create_dir_all(&import).unwrap();
        fs::write(import.join("hello.txt"), b"hello").unwrap();
        let fs = CensorFs::initialize(InitOptions {
            storage_root: storage.clone(),
            import_root: import,
            main_branch: BranchId::new("main").unwrap(),
        })
        .unwrap();
        let locked = run_fsck(&storage, false).unwrap_err();
        assert_eq!(locked.code, crate::ErrorCode::StateChanged);
        drop(fs);
        assert!(run_fsck(&storage, false).unwrap().clean);

        let fs = CensorFs::open(&storage).unwrap();
        fs.begin_exploration(RequestId::new(), "uid:1000", BranchId::new("main").unwrap())
            .unwrap();
        drop(fs);
        let report = run_fsck(&storage, false).unwrap();
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "open_ticket"));
        let repaired = run_fsck(&storage, true).unwrap();
        assert!(repaired.clean, "{:#?}", repaired.issues);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repairs_journal_tail_and_one_damaged_superblock_slot() {
        std::env::set_var("CENSORFS_ALLOW_UNSUPPORTED_BACKING", "1");
        let root = tempfile::TempDir::new().unwrap();
        let import = root.path().join("import");
        let storage = root.path().join("store");
        fs::create_dir(&import).unwrap();
        fs::write(import.join("file"), b"data").unwrap();
        let fs = CensorFs::initialize(InitOptions {
            storage_root: storage.clone(),
            import_root: import,
            main_branch: BranchId::new("main").unwrap(),
        })
        .unwrap();
        drop(fs);
        drop(CensorFs::open(&storage).unwrap());

        fs::write(storage.join("superblock/slot.a"), b"damaged").unwrap();
        use std::io::Write;
        let journal_path = storage.join("journal/current.log");
        let mut journal = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal_path)
            .unwrap();
        journal.write_all(b"partial").unwrap();
        drop(journal);
        let damaged_len = fs::metadata(&journal_path).unwrap().len();

        let report = run_fsck(&storage, false).unwrap();
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "invalid_slot"));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "journal_tail"));
        assert_eq!(fs::metadata(&journal_path).unwrap().len(), damaged_len);
        let repaired = run_fsck(&storage, true).unwrap();
        assert!(repaired.clean, "{:#?}", repaired.issues);
        assert!(fs::metadata(&journal_path).unwrap().len() < damaged_len);
    }

    #[test]
    fn damaged_object_is_reported_but_never_repaired() {
        std::env::set_var("CENSORFS_ALLOW_UNSUPPORTED_BACKING", "1");
        let root = tempfile::TempDir::new().unwrap();
        let import = root.path().join("import");
        let storage = root.path().join("store");
        fs::create_dir(&import).unwrap();
        fs::write(import.join("file"), b"data").unwrap();
        let fs = CensorFs::initialize(InitOptions {
            storage_root: storage.clone(),
            import_root: import,
            main_branch: BranchId::new("main").unwrap(),
        })
        .unwrap();
        let head = fs.branch_head(&BranchId::new("main").unwrap()).unwrap();
        let object = fs
            .manifest(head.generation_id)
            .unwrap()
            .entries
            .get(&LogicalPath::from_utf8("file").unwrap())
            .unwrap()
            .object_id
            .unwrap();
        drop(fs);
        let object_path = storage.join("objects").join(format!("{object}.data"));
        let mut bytes = fs::read(&object_path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&object_path, bytes).unwrap();

        let report = run_fsck(&storage, false).unwrap();
        assert!(report.issues.iter().any(|issue| issue.code == "object"));
        assert!(!run_fsck(&storage, true).unwrap().clean);
    }
}
