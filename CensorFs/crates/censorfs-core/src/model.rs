use crate::error::{Result, CensorFsError};
use crate::ids::*;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

pub type Digest = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct LogicalPath(Vec<u8>);

impl LogicalPath {
    pub fn root() -> Self {
        Self(Vec::new())
    }

    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();
        if bytes.len() > 4096 {
            return Err(CensorFsError::bad_request("logical path exceeds 4096 bytes"));
        }
        if bytes.first() == Some(&b'/') || bytes.last() == Some(&b'/') || bytes.contains(&0) {
            return Err(CensorFsError::bad_request(
                "logical path must be relative and contain no NUL",
            ));
        }
        if !bytes.is_empty() {
            for part in bytes.split(|b| *b == b'/') {
                if part.is_empty() || part.len() > 255 || part == b"." || part == b".." {
                    return Err(CensorFsError::bad_request(
                        "logical path has an invalid component",
                    ));
                }
            }
        }
        Ok(Self(bytes))
    }

    pub fn from_utf8(value: &str) -> Result<Self> {
        Self::new(value.as_bytes().to_vec())
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let end = self.0.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
        Some(Self(if end == 0 {
            Vec::new()
        } else {
            self.0[..end].to_vec()
        }))
    }
    pub fn file_name(&self) -> &[u8] {
        self.0.rsplit(|b| *b == b'/').next().unwrap_or_default()
    }
    pub fn starts_with_dir(&self, parent: &Self) -> bool {
        parent.is_root()
            || (self.0.starts_with(&parent.0) && self.0.get(parent.0.len()) == Some(&b'/'))
    }
}

impl Display for LogicalPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.0))
    }
}

impl<'de> Deserialize<'de> for LogicalPath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        Self::new(bytes).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsMode {
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Superblock {
    pub instance_id: InstanceId,
    pub format_version: u32,
    pub mount_seq: u64,
    pub mode: FsMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchHead {
    pub generation_id: GenerationId,
    pub head_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchMeta {
    pub branch_id: BranchId,
    pub created_from: GenerationId,
    pub created_by_request: Option<RequestId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    pub branch_id: BranchId,
    pub head: BranchHead,
    pub created_from: GenerationId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenerationKind {
    Initial,
    Normal,
    Rollback,
    Merge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationMeta {
    pub generation_id: GenerationId,
    pub manifest_id: ManifestId,
    pub parents: Vec<GenerationId>,
    pub source_ticket: Option<TicketId>,
    pub source_candidate: Option<CandidateId>,
    pub kind: GenerationKind,
    pub rollback_target: Option<GenerationId>,
    pub manifest_digest: Digest,
    pub entry_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: LogicalPath,
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    pub atime_ns: u64,
    pub mtime_ns: u64,
    pub object_id: Option<ObjectId>,
    pub content_digest: Option<Digest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub generation_id: GenerationId,
    pub entries: BTreeMap<LogicalPath, ManifestEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxState {
    Open,
    Closed,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxMeta {
    pub tx_id: TxId,
    pub actor_id: String,
    pub state: TxState,
    pub tickets: Vec<TicketId>,
    pub created_by_request: RequestId,
    pub terminal_request: Option<RequestId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketState {
    Open,
    Freezing,
    Prepared,
    Published,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketMeta {
    pub ticket_id: TicketId,
    pub tx_id: TxId,
    pub branch_id: BranchId,
    pub base_generation: GenerationId,
    pub expected_head_seq: u64,
    pub state: TicketState,
    pub candidate_id: Option<CandidateId>,
    pub created_by_request: RequestId,
    pub terminal_request: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlayEntry {
    File {
        storage_name: String,
        mode: u32,
        atime_ns: u64,
        mtime_ns: u64,
        size: u64,
    },
    Directory {
        mode: u32,
        atime_ns: u64,
        mtime_ns: u64,
    },
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeltaOp {
    Upsert,
    Delete,
    Rename,
    Mkdir,
    Rmdir,
    Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaRecord {
    pub seq: u64,
    pub op: DeltaOp,
    pub path: LogicalPath,
    pub new_path: Option<LogicalPath>,
    pub overlay: Option<OverlayEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateState {
    Prepared,
    Published,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateMeta {
    pub candidate_id: CandidateId,
    pub ticket_id: Option<TicketId>,
    pub tx_id: TxId,
    pub branch_id: BranchId,
    pub base_generation: GenerationId,
    pub expected_head_seq: u64,
    pub result_generation: GenerationId,
    pub changed_paths_digest: Digest,
    pub state: CandidateState,
    pub rollback_target: Option<GenerationId>,
    pub created_by_request: RequestId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishReceipt {
    pub branch_id: BranchId,
    pub head_seq: u64,
    pub old_generation: GenerationId,
    pub new_generation: GenerationId,
    pub candidate_id: CandidateId,
    pub ticket_id: Option<TicketId>,
    pub tx_id: TxId,
    pub decision_id: String,
    pub request_id: RequestId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewKind {
    Stable,
    Ticket,
    Candidate,
    Historical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewState {
    Created,
    Mounted,
    Revoked,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewHandle {
    pub view_id: ViewId,
    pub kind: ViewKind,
    pub generation_id: GenerationId,
    pub ticket_id: Option<TicketId>,
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub can_write: bool,
    pub mount_epoch: u64,
    pub state: ViewState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginTicketResult {
    pub ticket: TicketMeta,
    pub view: ViewHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareResult {
    pub candidate: CandidateMeta,
    pub generation: GenerationMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationResult {
    pub tx: TxMeta,
    pub ticket: TicketMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitResult {
    pub ticket: TicketMeta,
    pub candidate: CandidateMeta,
    pub generation: GenerationMeta,
    pub receipt: PublishReceipt,
    pub tx: TxMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbortResult {
    pub ticket: TicketMeta,
    pub tx: TxMeta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeConflictReason {
    BothModified,
    DeleteModify,
    TypeChanged,
    ParentMissingOrNotDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeConflict {
    pub path: LogicalPath,
    pub reason: MergeConflictReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeCheckResult {
    pub source_branch: BranchId,
    pub target_branch: BranchId,
    pub source_head: BranchHead,
    pub target_head: BranchHead,
    pub merge_base: GenerationId,
    pub conflicts: Vec<MergeConflict>,
    pub already_up_to_date: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeIntent {
    pub candidate_id: CandidateId,
    pub source_branch: BranchId,
    pub target_branch: BranchId,
    pub source_head: BranchHead,
    pub target_head: BranchHead,
    pub merge_base: GenerationId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePrepareResult {
    pub check: MergeCheckResult,
    pub intent: MergeIntent,
    pub candidate: CandidateMeta,
    pub generation: GenerationMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    pub check: MergeCheckResult,
    pub prepared: Option<MergePrepareResult>,
    pub receipt: Option<PublishReceipt>,
    pub tx: Option<TxMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffKind {
    Added,
    Deleted,
    TypeChanged,
    ContentChanged,
    MetadataChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathDiff {
    pub path: LogicalPath,
    pub kind: DiffKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextDiffDisposition {
    Unified,
    Binary,
    TooLarge,
    ResponseLimit,
    MetadataOnly,
    NonFile,
    TypeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPathDiff {
    pub path: LogicalPath,
    pub kind: DiffKind,
    pub disposition: TextDiffDisposition,
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
    pub old_digest: Option<String>,
    pub new_digest: Option<String>,
    pub patch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDiffReport {
    pub left: GenerationId,
    pub right: GenerationId,
    pub files: Vec<TextPathDiff>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JournalKind {
    BranchCreated,
    CandidateReady,
    RollbackCandidateReady,
    HeadSwitchPrepare,
    HeadSwitchCommit,
    TicketAborted,
    MergeCandidateReady,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalRecord {
    pub seq: u64,
    pub operation_id: RequestId,
    pub kind: JournalKind,
    pub target_id: String,
    pub payload_digest: Digest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedResponse {
    pub operation: String,
    pub payload: Vec<u8>,
}
