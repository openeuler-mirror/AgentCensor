pub use crate::api_generated::{ControlRequest, ControlResponse, Operation, RequestContext};
use crate::branch::RecoveryReport;
use crate::codec;
use crate::error::{ErrorCode, Result, CensorFsError};
use crate::ids::*;
use crate::model::*;
use crate::CensorFs;
use prost::Message;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FILE_CONTENT: usize = 1024 * 1024;
pub const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsInfo {
    pub superblock: Superblock,
    pub recovery: RecoveryReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestResultRequest {
    pub request_id: RequestId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxRequest {
    pub tx_id: TxId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ViewSelector {
    Branch(BranchId),
    Generation(GenerationId),
    Candidate(CandidateId),
    Ticket(TicketId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenViewRequest {
    pub selector: ViewSelector,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseViewRequest {
    pub view_id: ViewId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBranchRequest {
    pub branch_id: BranchId,
    pub base_generation: GenerationId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchRequest {
    pub branch_id: BranchId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBranchFromRequest {
    pub branch_id: BranchId,
    pub from_branch: BranchId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreRequest {
    pub branch_id: BranchId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitExplorationRequest {
    pub ticket_id: TicketId,
    pub message: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeBranchesRequest {
    pub source_branch: BranchId,
    pub target_branch: BranchId,
    pub message: String,
    pub check_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewPathRequest {
    pub selector: ViewSelector,
    pub path: LogicalPath,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketWriteRequest {
    pub ticket_id: TicketId,
    pub path: LogicalPath,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketPathRequest {
    pub ticket_id: TicketId,
    pub path: LogicalPath,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketMoveRequest {
    pub ticket_id: TicketId,
    pub source: LogicalPath,
    pub target: LogicalPath,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginTicketRequest {
    pub tx_id: TxId,
    pub branch_id: BranchId,
    pub expected_head: Option<BranchHead>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketRequest {
    pub ticket_id: TicketId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareTicketRequest {
    pub ticket_id: TicketId,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRequest {
    pub candidate_id: CandidateId,
    pub decision_id: String,
    pub expected_head: BranchHead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackRequest {
    pub tx_id: TxId,
    pub branch_id: BranchId,
    pub target_generation: GenerationId,
    pub expected_head: BranchHead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationRequest {
    pub generation_id: GenerationId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveForkPointRequest {
    pub tx_id: TxId,
    pub ticket_id: TicketId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffRequest {
    pub left: GenerationId,
    pub right: GenerationId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDiffRequest {
    pub left: GenerationId,
    pub right: GenerationId,
    pub max_file_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantOpenRequest {
    pub branch_id: BranchId,
    pub expected_head: BranchHead,
    pub run_id: String,
    pub variant_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantOpenResult {
    pub run_id: String,
    pub variant_id: String,
    pub expected_head: BranchHead,
    pub tx: TxMeta,
    pub ticket: TicketMeta,
    pub view: ViewHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPrepareRequest {
    pub ticket_id: TicketId,
    pub view_id: Option<ViewId>,
    pub run_id: String,
    pub variant_id: String,
    pub timeout_ms: u64,
    pub max_diff_file_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPrepareResult {
    pub run_id: String,
    pub variant_id: String,
    pub ticket: TicketMeta,
    pub candidate: CandidateMeta,
    pub generation: GenerationMeta,
    pub path_diff: Vec<PathDiff>,
    pub text_diff: TextDiffReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPublishRequest {
    pub candidate_id: CandidateId,
    pub expected_head: BranchHead,
    pub decision_id: String,
    pub run_id: String,
    pub variant_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPublishResult {
    pub run_id: String,
    pub variant_id: String,
    pub receipt: PublishReceipt,
    pub tx: TxMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantAbortRequest {
    pub ticket_id: TicketId,
    pub view_id: Option<ViewId>,
    pub run_id: String,
    pub variant_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantAbortResult {
    pub run_id: String,
    pub variant_id: String,
    pub ticket: TicketMeta,
    pub tx: TxMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachFuseRequest {
    pub view_id: ViewId,
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub read_only: bool,
}

#[derive(Debug, Clone)]
pub struct ControlDispatcher {
    fs: CensorFs,
}

impl ControlDispatcher {
    pub fn new(fs: CensorFs) -> Self {
        Self { fs }
    }

    fn require_tx_owner(&self, peer: PeerCredentials, tx_id: TxId) -> Result<()> {
        if peer.uid == 0 {
            return Ok(());
        }
        let tx = self.fs.tx(tx_id)?;
        if tx.actor_id == format!("uid:{}", peer.uid) {
            Ok(())
        } else {
            Err(CensorFsError::new(
                ErrorCode::AccessDenied,
                "transaction belongs to another Unix user",
            ))
        }
    }

    fn open_selector_view(
        &self,
        peer: PeerCredentials,
        selector: ViewSelector,
    ) -> Result<ViewHandle> {
        match selector {
            ViewSelector::Branch(branch) => self.fs.open_branch_view(&branch, peer.uid, peer.gid),
            ViewSelector::Generation(generation) => {
                self.fs.open_generation_view(generation, peer.uid, peer.gid)
            }
            ViewSelector::Candidate(candidate) => {
                self.require_tx_owner(peer, self.fs.candidate(candidate)?.tx_id)?;
                self.fs.open_candidate_view(candidate, peer.uid, peer.gid)
            }
            ViewSelector::Ticket(ticket) => {
                self.require_tx_owner(peer, self.fs.ticket(ticket)?.tx_id)?;
                self.fs.open_ticket_view(ticket, peer.uid, peer.gid)
            }
        }
    }

    fn with_selector_view<T>(
        &self,
        peer: PeerCredentials,
        selector: ViewSelector,
        action: impl FnOnce(&crate::viewfs::ViewEngine, ViewId) -> Result<T>,
    ) -> Result<T> {
        let view = self.open_selector_view(peer, selector)?;
        let result = action(self.fs.view_engine(), view.view_id);
        let close = self.fs.close_view(view.view_id, peer.uid);
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                close?;
                Ok(value)
            }
        }
    }

    pub fn dispatch(&self, peer: PeerCredentials, request: ControlRequest) -> ControlResponse {
        let request_id_bytes = request
            .context
            .as_ref()
            .map(|value| value.request_id.clone())
            .unwrap_or_default();
        match self.dispatch_inner(peer, request) {
            Ok(payload) => ControlResponse {
                request_id: request_id_bytes,
                status: 0,
                message: String::new(),
                payload,
            },
            Err(error) => ControlResponse {
                request_id: request_id_bytes,
                status: error.code as u32,
                message: error.message,
                payload: Vec::new(),
            },
        }
    }

    #[cfg(target_os = "linux")]
    fn dispatch_attached_fuse(
        &self,
        peer: PeerCredentials,
        request: ControlRequest,
        descriptor: Option<std::os::fd::OwnedFd>,
    ) -> ControlResponse {
        let request_id_bytes = request
            .context
            .as_ref()
            .map(|value| value.request_id.clone())
            .unwrap_or_default();
        let result = (|| {
            if peer.uid != 0 {
                return Err(CensorFsError::new(
                    ErrorCode::AccessDenied,
                    "FUSE attachment requires the privileged mounter",
                ));
            }
            let context = request
                .context
                .as_ref()
                .ok_or_else(|| CensorFsError::bad_request("missing request context"))?;
            if context.protocol_version != PROTOCOL_VERSION {
                return Err(CensorFsError::bad_request(
                    "unsupported control protocol version",
                ));
            }
            if context.deadline_unix_ms != 0 && unix_ms() > context.deadline_unix_ms {
                return Err(CensorFsError::bad_request("request deadline has expired"));
            }
            let _ = request_id(&context.request_id)?;
            if context.mount_epoch != 0 && context.mount_epoch != self.fs.superblock().mount_seq {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "mount epoch does not match the running daemon",
                ));
            }
            let input: AttachFuseRequest = decode(&request.payload)?;
            let view = self.fs.view_engine().view(input.view_id)?;
            if view.owner_uid != input.owner_uid
                || view.owner_gid != input.owner_gid
                || view.can_write == input.read_only
            {
                return Err(CensorFsError::new(
                    ErrorCode::AccessDenied,
                    "mount parameters do not match the view",
                ));
            }
            let descriptor = descriptor.ok_or_else(|| {
                CensorFsError::bad_request("ATTACH_FUSE requires a passed file descriptor")
            })?;
            crate::fuse_adapter::spawn_session(
                self.fs.view_engine_arc(),
                input.view_id,
                descriptor,
            )?;
            encode(
                &self
                    .fs
                    .view_engine()
                    .mark_mounted(input.view_id, input.owner_uid)?,
            )
        })();
        match result {
            Ok(payload) => ControlResponse {
                request_id: request_id_bytes,
                status: 0,
                message: String::new(),
                payload,
            },
            Err(error) => ControlResponse {
                request_id: request_id_bytes,
                status: error.code as u32,
                message: error.message,
                payload: Vec::new(),
            },
        }
    }

    fn dispatch_inner(&self, peer: PeerCredentials, request: ControlRequest) -> Result<Vec<u8>> {
        let context = request
            .context
            .ok_or_else(|| CensorFsError::bad_request("missing request context"))?;
        if context.protocol_version != PROTOCOL_VERSION {
            return Err(CensorFsError::bad_request(
                "unsupported control protocol version",
            ));
        }
        if context.deadline_unix_ms != 0 && unix_ms() > context.deadline_unix_ms {
            return Err(CensorFsError::bad_request("request deadline has expired"));
        }
        if context.mount_epoch != 0 && context.mount_epoch != self.fs.superblock().mount_seq {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                "mount epoch does not match the running daemon",
            ));
        }
        let request_id = request_id(&context.request_id)?;
        let operation = Operation::try_from(request.operation)
            .map_err(|_| CensorFsError::bad_request("unknown operation"))?;
        match operation {
            Operation::GetFsInfo => encode(&FsInfo {
                superblock: self.fs.superblock(),
                recovery: self.fs.recovery_report(),
            }),
            Operation::InspectRecovery => encode(&self.fs.recovery_report()),
            Operation::GetRequestResult => {
                let input: RequestResultRequest = decode(&request.payload)?;
                encode(&self.fs.request_result(input.request_id)?)
            }
            Operation::BeginTx => {
                let tx = self.fs.begin_tx(request_id, format!("uid:{}", peer.uid))?;
                self.require_tx_owner(peer, tx.tx_id)?;
                encode(&tx)
            }
            Operation::CloseTx => {
                let input: TxRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, input.tx_id)?;
                encode(&self.fs.close_tx(request_id, input.tx_id)?)
            }
            Operation::AbortTx => {
                let input: TxRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, input.tx_id)?;
                encode(&self.fs.abort_tx(request_id, input.tx_id)?)
            }
            Operation::OpenView => {
                let input: OpenViewRequest = decode(&request.payload)?;
                encode(&self.open_selector_view(peer, input.selector)?)
            }
            Operation::CloseView => {
                let input: CloseViewRequest = decode(&request.payload)?;
                encode(&self.fs.close_view(input.view_id, peer.uid)?)
            }
            Operation::CreateBranch => {
                let input: CreateBranchRequest = decode(&request.payload)?;
                encode(&self.fs.create_branch(
                    request_id,
                    input.branch_id,
                    input.base_generation,
                )?)
            }
            Operation::GetBranchHead => {
                let input: BranchRequest = decode(&request.payload)?;
                encode(&self.fs.branch_head(&input.branch_id)?)
            }
            Operation::ListBranches => encode(&self.fs.branches()),
            Operation::CreateBranchFrom => {
                let input: CreateBranchFromRequest = decode(&request.payload)?;
                let base = self.fs.branch_head(&input.from_branch)?;
                encode(
                    &self
                        .fs
                        .create_branch(request_id, input.branch_id, base.generation_id)?,
                )
            }
            Operation::BeginTicket => {
                let input: BeginTicketRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, input.tx_id)?;
                encode(&self.fs.begin_ticket(
                    request_id,
                    input.tx_id,
                    input.branch_id,
                    input.expected_head,
                )?)
            }
            Operation::PrepareTicket => {
                let input: PrepareTicketRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                encode(&self.fs.prepare_ticket(
                    request_id,
                    input.ticket_id,
                    Duration::from_millis(input.timeout_ms),
                )?)
            }
            Operation::AbortTicket => {
                let input: TicketRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                encode(&self.fs.abort_ticket(request_id, input.ticket_id)?)
            }
            Operation::Explore => {
                let input: ExploreRequest = decode(&request.payload)?;
                encode(&self.fs.begin_exploration(
                    request_id,
                    format!("uid:{}", peer.uid),
                    input.branch_id,
                )?)
            }
            Operation::AbortExploration => {
                let input: TicketRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                encode(&self.fs.abort_exploration(request_id, input.ticket_id)?)
            }
            Operation::Publish => {
                let input: PublishRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.candidate(input.candidate_id)?.tx_id)?;
                encode(&self.fs.publish(
                    request_id,
                    input.candidate_id,
                    input.decision_id,
                    input.expected_head,
                )?)
            }
            Operation::BuildRollbackCandidate => {
                let input: RollbackRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, input.tx_id)?;
                encode(&self.fs.build_rollback_candidate(
                    request_id,
                    input.tx_id,
                    input.branch_id,
                    input.target_generation,
                    input.expected_head,
                )?)
            }
            Operation::CommitExploration => {
                let input: CommitExplorationRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                encode(&self.fs.commit_exploration(
                    request_id,
                    input.ticket_id,
                    input.message,
                    Duration::from_millis(input.timeout_ms),
                )?)
            }
            Operation::GetGenerationInfo => {
                let input: GenerationRequest = decode(&request.payload)?;
                encode(&self.fs.generation(input.generation_id)?)
            }
            Operation::ResolveForkPoint => {
                let input: ResolveForkPointRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, input.tx_id)?;
                encode(&self.fs.resolve_fork_point(input.tx_id, input.ticket_id)?)
            }
            Operation::DiffGenerations => {
                let input: DiffRequest = decode(&request.payload)?;
                encode(&self.fs.diff(input.left, input.right)?)
            }
            Operation::DiffText => {
                let input: TextDiffRequest = decode(&request.payload)?;
                let max_file_bytes = checked_diff_limit(input.max_file_bytes)?;
                encode(&bounded_text_diff(self.fs.text_diff(
                    input.left,
                    input.right,
                    max_file_bytes,
                )?))
            }
            Operation::MergeBranches => {
                let input: MergeBranchesRequest = decode(&request.payload)?;
                encode(&self.fs.merge_branches(
                    request_id,
                    format!("uid:{}", peer.uid),
                    input.source_branch,
                    input.target_branch,
                    input.message,
                    input.check_only,
                )?)
            }
            Operation::TestList => {
                let input: ViewPathRequest = decode(&request.payload)?;
                encode(
                    &self.with_selector_view(peer, input.selector, |engine, view| {
                        engine.readdir(view, &input.path)
                    })?,
                )
            }
            Operation::TestCat => {
                let input: ViewPathRequest = decode(&request.payload)?;
                let contents = self.with_selector_view(peer, input.selector, |engine, view| {
                    engine.read_file(view, &input.path)
                })?;
                if contents.len() > MAX_FILE_CONTENT {
                    return Err(CensorFsError::bad_request(
                        "file content exceeds the 1 MiB test RPC limit",
                    ));
                }
                encode(&contents)
            }
            Operation::TestWrite => {
                let input: TicketWriteRequest = decode(&request.payload)?;
                if input.data.len() > MAX_FILE_CONTENT {
                    return Err(CensorFsError::bad_request(
                        "file content exceeds the 1 MiB test RPC limit",
                    ));
                }
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                self.with_selector_view(
                    peer,
                    ViewSelector::Ticket(input.ticket_id),
                    |engine, view| engine.write_file(view, input.path, &input.data),
                )?;
                encode(&())
            }
            Operation::TestMkdir => {
                let input: TicketPathRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                self.with_selector_view(
                    peer,
                    ViewSelector::Ticket(input.ticket_id),
                    |engine, view| engine.mkdir(view, input.path, 0o755),
                )?;
                encode(&())
            }
            Operation::TestRemove => {
                let input: TicketPathRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                self.with_selector_view(
                    peer,
                    ViewSelector::Ticket(input.ticket_id),
                    |engine, view| {
                        let entry = engine.lookup(view, &input.path)?;
                        match entry.kind {
                            EntryKind::File => engine.unlink(view, input.path),
                            EntryKind::Directory => engine.rmdir(view, input.path),
                        }
                    },
                )?;
                encode(&())
            }
            Operation::TestMove => {
                let input: TicketMoveRequest = decode(&request.payload)?;
                self.require_tx_owner(peer, self.fs.ticket(input.ticket_id)?.tx_id)?;
                self.with_selector_view(
                    peer,
                    ViewSelector::Ticket(input.ticket_id),
                    |engine, view| engine.rename(view, input.source, input.target),
                )?;
                encode(&())
            }
            Operation::VariantOpen => {
                let input: VariantOpenRequest = decode(&request.payload)?;
                validate_variant_identity(&input.run_id, &input.variant_id)?;
                encode(&self.fs.idempotent_with_input(
                    request_id,
                    "variant_open",
                    &input,
                    || {
                        let current = self.fs.branch_head(&input.branch_id)?;
                        if current != input.expected_head {
                            return Err(CensorFsError::new(
                                ErrorCode::HeadChanged,
                                format!(
                                    "current head is {}/{}",
                                    current.generation_id, current.head_seq
                                ),
                            ));
                        }
                        let tx = self
                            .fs
                            .begin_tx(request_id.derive("begin_tx"), format!("uid:{}", peer.uid))?;
                        self.require_tx_owner(peer, tx.tx_id)?;
                        let ticket = self.fs.begin_ticket(
                            request_id.derive("begin_ticket"),
                            tx.tx_id,
                            input.branch_id.clone(),
                            Some(input.expected_head.clone()),
                        )?;
                        let view =
                            self.fs
                                .open_ticket_view(ticket.ticket_id, peer.uid, peer.gid)?;
                        Ok(VariantOpenResult {
                            run_id: input.run_id.clone(),
                            variant_id: input.variant_id.clone(),
                            expected_head: input.expected_head.clone(),
                            tx,
                            ticket,
                            view,
                        })
                    },
                )?)
            }
            Operation::VariantPrepare => {
                let input: VariantPrepareRequest = decode(&request.payload)?;
                validate_variant_identity(&input.run_id, &input.variant_id)?;
                encode(&self.fs.idempotent_with_input(
                    request_id,
                    "variant_prepare",
                    &input,
                    || {
                        let original = self.fs.ticket(input.ticket_id)?;
                        self.require_tx_owner(peer, original.tx_id)?;
                        close_view_if_present(&self.fs, peer.uid, input.view_id)?;
                        let prepared = self.fs.prepare_ticket(
                            request_id.derive("prepare_ticket"),
                            input.ticket_id,
                            Duration::from_millis(input.timeout_ms),
                        )?;
                        let max_file_bytes = checked_diff_limit(input.max_diff_file_bytes)?;
                        let path_diff = self
                            .fs
                            .diff(original.base_generation, prepared.generation.generation_id)?;
                        let text_diff = bounded_text_diff(self.fs.text_diff(
                            original.base_generation,
                            prepared.generation.generation_id,
                            max_file_bytes,
                        )?);
                        Ok(VariantPrepareResult {
                            run_id: input.run_id.clone(),
                            variant_id: input.variant_id.clone(),
                            ticket: self.fs.ticket(input.ticket_id)?,
                            candidate: prepared.candidate,
                            generation: prepared.generation,
                            path_diff,
                            text_diff,
                        })
                    },
                )?)
            }
            Operation::VariantPublish => {
                let input: VariantPublishRequest = decode(&request.payload)?;
                validate_variant_identity(&input.run_id, &input.variant_id)?;
                if input.decision_id.is_empty() || input.decision_id.len() > 512 {
                    return Err(CensorFsError::bad_request(
                        "decision id must contain 1..=512 bytes",
                    ));
                }
                encode(&self.fs.idempotent_with_input(
                    request_id,
                    "variant_publish",
                    &input,
                    || {
                        let candidate = self.fs.candidate(input.candidate_id)?;
                        self.require_tx_owner(peer, candidate.tx_id)?;
                        let receipt = self.fs.publish(
                            request_id.derive("publish"),
                            input.candidate_id,
                            input.decision_id.clone(),
                            input.expected_head.clone(),
                        )?;
                        let tx = self
                            .fs
                            .close_tx(request_id.derive("close_tx"), candidate.tx_id)?;
                        Ok(VariantPublishResult {
                            run_id: input.run_id.clone(),
                            variant_id: input.variant_id.clone(),
                            receipt,
                            tx,
                        })
                    },
                )?)
            }
            Operation::VariantAbort => {
                let input: VariantAbortRequest = decode(&request.payload)?;
                validate_variant_identity(&input.run_id, &input.variant_id)?;
                encode(&self.fs.idempotent_with_input(
                    request_id,
                    "variant_abort",
                    &input,
                    || {
                        let original = self.fs.ticket(input.ticket_id)?;
                        self.require_tx_owner(peer, original.tx_id)?;
                        close_view_if_present(&self.fs, peer.uid, input.view_id)?;
                        let ticket = self
                            .fs
                            .abort_ticket(request_id.derive("abort_ticket"), input.ticket_id)?;
                        let tx = self
                            .fs
                            .abort_tx(request_id.derive("abort_tx"), original.tx_id)?;
                        Ok(VariantAbortResult {
                            run_id: input.run_id.clone(),
                            variant_id: input.variant_id.clone(),
                            ticket,
                            tx,
                        })
                    },
                )?)
            }
            Operation::AttachFuse => Err(CensorFsError::bad_request(
                "ATTACH_FUSE requires the privileged descriptor handoff",
            )),
            Operation::Unspecified => Err(CensorFsError::bad_request("operation is unspecified")),
        }
    }
}

fn validate_variant_identity(run_id: &str, variant_id: &str) -> Result<()> {
    for (kind, value) in [("run", run_id), ("variant", variant_id)] {
        if value.is_empty()
            || value.len() > 128
            || value.chars().any(|character| character.is_control())
        {
            return Err(CensorFsError::bad_request(format!(
                "{kind} id must contain 1..=128 bytes and no control characters"
            )));
        }
    }
    Ok(())
}

fn checked_diff_limit(value: u64) -> Result<usize> {
    if value == 0 || value > MAX_FILE_CONTENT as u64 {
        return Err(CensorFsError::bad_request(
            "max diff file bytes must be in 1..=1048576",
        ));
    }
    Ok(value as usize)
}

fn bounded_text_diff(mut report: TextDiffReport) -> TextDiffReport {
    let mut used = 0_usize;
    for file in &mut report.files {
        let Some(patch) = &file.patch else {
            continue;
        };
        if used.saturating_add(patch.len()) > MAX_FILE_CONTENT {
            file.patch = None;
            file.disposition = TextDiffDisposition::ResponseLimit;
        } else {
            used += patch.len();
        }
    }
    report
}

fn close_view_if_present(fs: &CensorFs, caller_uid: u32, view_id: Option<ViewId>) -> Result<()> {
    let Some(view_id) = view_id else {
        return Ok(());
    };
    match fs.close_view(view_id, caller_uid) {
        Ok(_) => Ok(()),
        Err(error) if error.code == ErrorCode::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    codec::serialize(value)
}
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    codec::deserialize(bytes)
}

pub fn request<T: Serialize>(
    operation: Operation,
    request_id: RequestId,
    mount_epoch: u64,
    payload: &T,
) -> Result<ControlRequest> {
    Ok(ControlRequest {
        context: Some(RequestContext {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.0.as_bytes().to_vec(),
            actor_id: String::new(),
            mount_epoch,
            deadline_unix_ms: 0,
        }),
        operation: operation as i32,
        payload: encode(payload)?,
    })
}

pub fn write_frame(mut writer: impl Write, message: &impl Message) -> Result<()> {
    let len = message.encoded_len();
    if len > MAX_FRAME_SIZE {
        return Err(CensorFsError::bad_request("control frame exceeds 2 MiB"));
    }
    writer.write_all(&(len as u32).to_le_bytes())?;
    let mut body = Vec::with_capacity(len);
    message
        .encode(&mut body)
        .map_err(|error| CensorFsError::bad_request(error.to_string()))?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<M: Message + Default>(mut reader: impl Read) -> Result<Option<M>> {
    let mut header = [0u8; 4];
    let first = reader.read(&mut header[..1])?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..])?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(CensorFsError::bad_request("control frame exceeds 2 MiB"));
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    M::decode(body.as_slice())
        .map(Some)
        .map_err(|error| CensorFsError::bad_request(error.to_string()))
}

fn request_id(bytes: &[u8]) -> Result<RequestId> {
    let uuid = uuid::Uuid::from_slice(bytes)
        .map_err(|_| CensorFsError::bad_request("request_id must be a 16-byte UUID"))?;
    Ok(RequestId(uuid))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(target_os = "linux")]
pub struct ControlServer {
    socket_path: PathBuf,
    dispatcher: Arc<ControlDispatcher>,
}

#[cfg(target_os = "linux")]
impl ControlServer {
    pub fn new(socket_path: impl Into<PathBuf>, fs: CensorFs) -> Self {
        Self {
            socket_path: socket_path.into(),
            dispatcher: Arc::new(ControlDispatcher::new(fs)),
        }
    }

    pub fn run(self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if self.socket_path.exists() {
            if std::os::unix::net::UnixStream::connect(&self.socket_path).is_ok() {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "control socket is already active",
                ));
            }
            std::fs::remove_file(&self.socket_path)?;
        }
        let listener = UnixListener::bind(&self.socket_path)?;
        std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o660))?;
        for stream in listener.incoming() {
            let stream = stream?;
            let dispatcher = Arc::clone(&self.dispatcher);
            std::thread::spawn(move || {
                let _ = serve_connection(stream, &dispatcher);
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn serve_connection(
    mut stream: std::os::unix::net::UnixStream,
    dispatcher: &ControlDispatcher,
) -> Result<()> {
    let peer = peer_credentials(&stream)?;
    let Some((request, descriptor)) = read_request_with_descriptor(&mut stream)? else {
        return Ok(());
    };
    let response = if Operation::try_from(request.operation).ok() == Some(Operation::AttachFuse) {
        dispatcher.dispatch_attached_fuse(peer, request, descriptor)
    } else {
        if descriptor.is_some() {
            ControlResponse {
                request_id: Vec::new(),
                status: ErrorCode::BadRequest as u32,
                message: "unexpected passed file descriptor".into(),
                payload: Vec::new(),
            }
        } else {
            dispatcher.dispatch(peer, request)
        }
    };
    write_frame(&mut stream, &response)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_request_with_descriptor(
    stream: &mut std::os::unix::net::UnixStream,
) -> Result<Option<(ControlRequest, Option<std::os::fd::OwnedFd>)>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    #[repr(align(8))]
    struct ControlBuffer([u8; 64]);
    let mut bytes = vec![0u8; MAX_FRAME_SIZE + 4];
    let mut control = ControlBuffer([0; 64]);
    let mut iovec = libc::iovec {
        iov_base: bytes.as_mut_ptr() as *mut libc::c_void,
        iov_len: bytes.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.0.as_mut_ptr() as *mut libc::c_void;
    message.msg_controllen = control.0.len();
    let received =
        unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    if received < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if received == 0 {
        return Ok(None);
    }
    let mut descriptor = None;
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    if !header.is_null()
        && unsafe {
            (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS
        }
    {
        let raw = unsafe { *(libc::CMSG_DATA(header) as *const libc::c_int) };
        descriptor = Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) });
    }
    let mut received = received as usize;
    if received < 4 {
        stream.read_exact(&mut bytes[received..4])?;
        received = 4;
    }
    let body_len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if body_len > MAX_FRAME_SIZE {
        return Err(CensorFsError::bad_request("control frame exceeds 2 MiB"));
    }
    let frame_len = 4 + body_len;
    if received > frame_len {
        return Err(CensorFsError::bad_request(
            "multiple requests in a single control frame are not supported",
        ));
    }
    if received < frame_len {
        stream.read_exact(&mut bytes[received..frame_len])?;
    }
    let request = ControlRequest::decode(&bytes[4..frame_len])
        .map_err(|error| CensorFsError::bad_request(error.to_string()))?;
    Ok(Some((request, descriptor)))
}

#[cfg(target_os = "linux")]
fn peer_credentials(stream: &std::os::unix::net::UnixStream) -> Result<PeerCredentials> {
    use std::os::fd::AsRawFd;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(PeerCredentials {
        pid: credentials.pid as u32,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

#[derive(Debug, Clone)]
pub struct ControlClient {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    socket_path: PathBuf,
    mount_epoch: u64,
}

impl ControlClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            mount_epoch: 0,
        }
    }
    pub fn with_mount_epoch(mut self, mount_epoch: u64) -> Self {
        self.mount_epoch = mount_epoch;
        self
    }

    #[cfg(target_os = "linux")]
    pub fn call<I: Serialize, O: DeserializeOwned>(
        &self,
        operation: Operation,
        request_id: RequestId,
        input: &I,
    ) -> Result<O> {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.socket_path).map_err(
            |error| {
                CensorFsError::new(
                    ErrorCode::IoError,
                    format!(
                        "cannot connect to daemon socket {}: {error}; start censorfs-daemon and verify CENSORFS_SOCKET",
                        self.socket_path.display()
                    ),
                )
            },
        )?;
        write_frame(
            &mut stream,
            &request(operation, request_id, self.mount_epoch, input)?,
        )?;
        let response = read_frame::<ControlResponse>(&mut stream)?.ok_or_else(|| {
            CensorFsError::new(ErrorCode::IoError, "daemon closed the control socket")
        })?;
        if response.status != 0 {
            return Err(CensorFsError::new(
                error_code(response.status),
                response.message,
            ));
        }
        decode(&response.payload)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn call<I: Serialize, O: DeserializeOwned>(
        &self,
        _operation: Operation,
        _request_id: RequestId,
        _input: &I,
    ) -> Result<O> {
        Err(CensorFsError::new(
            ErrorCode::Unsupported,
            "Unix control sockets are only available on Linux",
        ))
    }
}

#[cfg(target_os = "linux")]
pub fn attach_fuse(
    socket_path: &Path,
    input: &AttachFuseRequest,
    descriptor: std::os::fd::RawFd,
) -> Result<ViewHandle> {
    use std::os::fd::AsRawFd;
    let request_id = RequestId::new();
    let message = request(Operation::AttachFuse, request_id, 0, input)?;
    let mut body = Vec::with_capacity(message.encoded_len());
    message
        .encode(&mut body)
        .map_err(|error| CensorFsError::bad_request(error.to_string()))?;
    if body.len() > MAX_FRAME_SIZE {
        return Err(CensorFsError::bad_request("control frame exceeds 2 MiB"));
    }
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path)?;
    send_descriptor(stream.as_raw_fd(), descriptor, &frame)?;
    let response = read_frame::<ControlResponse>(&mut stream)?
        .ok_or_else(|| CensorFsError::new(ErrorCode::IoError, "daemon closed the control socket"))?;
    if response.status != 0 {
        return Err(CensorFsError::new(
            error_code(response.status),
            response.message,
        ));
    }
    decode(&response.payload)
}

#[cfg(target_os = "linux")]
fn send_descriptor(
    socket: std::os::fd::RawFd,
    descriptor: std::os::fd::RawFd,
    frame: &[u8],
) -> Result<()> {
    #[repr(align(8))]
    struct ControlBuffer([u8; 64]);
    let mut control = ControlBuffer([0; 64]);
    let mut iovec = libc::iovec {
        iov_base: frame.as_ptr() as *mut libc::c_void,
        iov_len: frame.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.0.as_mut_ptr() as *mut libc::c_void;
    message.msg_controllen =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    if header.is_null() {
        return Err(CensorFsError::new(
            ErrorCode::IoError,
            "could not allocate SCM_RIGHTS header",
        ));
    }
    unsafe {
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize;
        *(libc::CMSG_DATA(header) as *mut libc::c_int) = descriptor;
    }
    let sent = unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL) };
    if sent < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if sent as usize != frame.len() {
        return Err(CensorFsError::new(
            ErrorCode::IoError,
            "short write while passing FUSE descriptor",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn error_code(value: u32) -> ErrorCode {
    match value {
        1 => ErrorCode::BadRequest,
        2 => ErrorCode::NotFound,
        3 => ErrorCode::AccessDenied,
        4 => ErrorCode::HeadChanged,
        5 => ErrorCode::StateChanged,
        6 => ErrorCode::BusyOpenWriters,
        7 => ErrorCode::AlreadyExistsDifferent,
        8 => ErrorCode::NoSpace,
        9 => ErrorCode::ReadOnly,
        10 => ErrorCode::IoError,
        11 => ErrorCode::ResultUnknown,
        12 => ErrorCode::Unsupported,
        13 => ErrorCode::Corrupt,
        14 => ErrorCode::Conflict,
        15 => ErrorCode::AlreadyUpToDate,
        _ => ErrorCode::Corrupt,
    }
}

pub fn default_socket(storage_root: &Path) -> PathBuf {
    storage_root.join("control.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protobuf_frame_round_trip_and_size_limit() {
        let message = ControlResponse {
            request_id: vec![1; 16],
            status: 0,
            message: "ok".into(),
            payload: vec![2; 8],
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        let decoded = read_frame::<ControlResponse>(&bytes[..]).unwrap().unwrap();
        assert_eq!(decoded, message);

        let oversized = ControlResponse {
            payload: vec![0; MAX_FRAME_SIZE + 1],
            ..Default::default()
        };
        assert!(write_frame(Vec::new(), &oversized).is_err());
    }
}
