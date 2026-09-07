use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::PathBuf;
use censorfs_core::control::*;
use censorfs_core::*;

type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Parser)]
#[command(name = "censorfsctl", about = "Unprivileged CensorFS control client")]
struct Arguments {
    #[arg(long, default_value = "/run/censorfs/control.sock", global = true)]
    socket: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init {
        #[arg(long)]
        storage_root: PathBuf,
        #[arg(long)]
        import_root: PathBuf,
        #[arg(long, default_value = "main")]
        branch: String,
    },
    Info,
    Recovery,
    BeginTx,
    CloseTx {
        tx_id: String,
    },
    AbortTx {
        tx_id: String,
    },
    BranchHead {
        branch: String,
    },
    CreateBranch {
        branch: String,
        base_generation: String,
    },
    OpenBranch {
        branch: String,
    },
    OpenGeneration {
        generation_id: String,
    },
    OpenCandidate {
        candidate_id: String,
    },
    OpenTicket {
        ticket_id: String,
    },
    CloseView {
        view_id: String,
    },
    BeginTicket {
        tx_id: String,
        branch: String,
    },
    Prepare {
        ticket_id: String,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
    },
    AbortTicket {
        ticket_id: String,
    },
    Publish {
        candidate_id: String,
        generation_id: String,
        head_seq: u64,
        decision_id: String,
    },
    Rollback {
        tx_id: String,
        branch: String,
        target_generation: String,
        current_generation: String,
        head_seq: u64,
    },
    Generation {
        generation_id: String,
    },
    ForkPoint {
        tx_id: String,
        ticket_id: String,
    },
    Diff {
        left: String,
        right: String,
    },
    RequestResult {
        request_id: String,
    },
}

fn parse_branch(value: String) -> AnyResult<BranchId> {
    BranchId::new(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error).into())
}

fn parse_id<T>(
    value: &str,
    parse: impl FnOnce(&str) -> std::result::Result<T, uuid::Error>,
) -> AnyResult<T> {
    Ok(parse(value)?)
}

fn call<I: Serialize, O: DeserializeOwned + Serialize>(
    client: &ControlClient,
    operation: Operation,
    input: &I,
) -> AnyResult<()> {
    let output: O = client.call(operation, RequestId::new(), input)?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn main() -> AnyResult<()> {
    let arguments = Arguments::parse();
    if let Command::Init {
        storage_root,
        import_root,
        branch,
    } = arguments.command
    {
        let fs = CensorFs::initialize(InitOptions {
            storage_root,
            import_root,
            main_branch: parse_branch(branch)?,
        })?;
        println!("{}", serde_json::to_string_pretty(&fs.recovery_report())?);
        return Ok(());
    }
    let client = ControlClient::new(arguments.socket);
    match arguments.command {
        Command::Init { .. } => unreachable!(),
        Command::Info => call::<_, FsInfo>(&client, Operation::GetFsInfo, &()),
        Command::Recovery => call::<_, censorfs_core::branch::RecoveryReport>(
            &client,
            Operation::InspectRecovery,
            &(),
        ),
        Command::BeginTx => call::<_, TxMeta>(&client, Operation::BeginTx, &()),
        Command::CloseTx { tx_id } => call::<_, TxMeta>(
            &client,
            Operation::CloseTx,
            &TxRequest {
                tx_id: parse_id(&tx_id, TxId::parse)?,
            },
        ),
        Command::AbortTx { tx_id } => call::<_, TxMeta>(
            &client,
            Operation::AbortTx,
            &TxRequest {
                tx_id: parse_id(&tx_id, TxId::parse)?,
            },
        ),
        Command::BranchHead { branch } => call::<_, BranchHead>(
            &client,
            Operation::GetBranchHead,
            &BranchRequest {
                branch_id: parse_branch(branch)?,
            },
        ),
        Command::CreateBranch {
            branch,
            base_generation,
        } => call::<_, BranchHead>(
            &client,
            Operation::CreateBranch,
            &CreateBranchRequest {
                branch_id: parse_branch(branch)?,
                base_generation: parse_id(&base_generation, GenerationId::parse)?,
            },
        ),
        Command::OpenBranch { branch } => call::<_, ViewHandle>(
            &client,
            Operation::OpenView,
            &OpenViewRequest {
                selector: ViewSelector::Branch(parse_branch(branch)?),
            },
        ),
        Command::OpenGeneration { generation_id } => call::<_, ViewHandle>(
            &client,
            Operation::OpenView,
            &OpenViewRequest {
                selector: ViewSelector::Generation(parse_id(&generation_id, GenerationId::parse)?),
            },
        ),
        Command::OpenCandidate { candidate_id } => call::<_, ViewHandle>(
            &client,
            Operation::OpenView,
            &OpenViewRequest {
                selector: ViewSelector::Candidate(parse_id(&candidate_id, CandidateId::parse)?),
            },
        ),
        Command::OpenTicket { ticket_id } => call::<_, ViewHandle>(
            &client,
            Operation::OpenView,
            &OpenViewRequest {
                selector: ViewSelector::Ticket(parse_id(&ticket_id, TicketId::parse)?),
            },
        ),
        Command::CloseView { view_id } => call::<_, ViewHandle>(
            &client,
            Operation::CloseView,
            &CloseViewRequest {
                view_id: parse_id(&view_id, ViewId::parse)?,
            },
        ),
        Command::BeginTicket { tx_id, branch } => call::<_, TicketMeta>(
            &client,
            Operation::BeginTicket,
            &BeginTicketRequest {
                tx_id: parse_id(&tx_id, TxId::parse)?,
                branch_id: parse_branch(branch)?,
                expected_head: None,
            },
        ),
        Command::Prepare {
            ticket_id,
            timeout_ms,
        } => call::<_, PrepareResult>(
            &client,
            Operation::PrepareTicket,
            &PrepareTicketRequest {
                ticket_id: parse_id(&ticket_id, TicketId::parse)?,
                timeout_ms,
            },
        ),
        Command::AbortTicket { ticket_id } => call::<_, TicketMeta>(
            &client,
            Operation::AbortTicket,
            &TicketRequest {
                ticket_id: parse_id(&ticket_id, TicketId::parse)?,
            },
        ),
        Command::Publish {
            candidate_id,
            generation_id,
            head_seq,
            decision_id,
        } => call::<_, PublishReceipt>(
            &client,
            Operation::Publish,
            &PublishRequest {
                candidate_id: parse_id(&candidate_id, CandidateId::parse)?,
                decision_id,
                expected_head: BranchHead {
                    generation_id: parse_id(&generation_id, GenerationId::parse)?,
                    head_seq,
                },
            },
        ),
        Command::Rollback {
            tx_id,
            branch,
            target_generation,
            current_generation,
            head_seq,
        } => call::<_, PrepareResult>(
            &client,
            Operation::BuildRollbackCandidate,
            &RollbackRequest {
                tx_id: parse_id(&tx_id, TxId::parse)?,
                branch_id: parse_branch(branch)?,
                target_generation: parse_id(&target_generation, GenerationId::parse)?,
                expected_head: BranchHead {
                    generation_id: parse_id(&current_generation, GenerationId::parse)?,
                    head_seq,
                },
            },
        ),
        Command::Generation { generation_id } => call::<_, GenerationMeta>(
            &client,
            Operation::GetGenerationInfo,
            &GenerationRequest {
                generation_id: parse_id(&generation_id, GenerationId::parse)?,
            },
        ),
        Command::ForkPoint { tx_id, ticket_id } => call::<_, GenerationId>(
            &client,
            Operation::ResolveForkPoint,
            &ResolveForkPointRequest {
                tx_id: parse_id(&tx_id, TxId::parse)?,
                ticket_id: parse_id(&ticket_id, TicketId::parse)?,
            },
        ),
        Command::Diff { left, right } => call::<_, Vec<PathDiff>>(
            &client,
            Operation::DiffGenerations,
            &DiffRequest {
                left: parse_id(&left, GenerationId::parse)?,
                right: parse_id(&right, GenerationId::parse)?,
            },
        ),
        Command::RequestResult { request_id } => call::<_, Option<PersistedResponse>>(
            &client,
            Operation::GetRequestResult,
            &RequestResultRequest {
                request_id: parse_id(&request_id, RequestId::parse)?,
            },
        ),
    }
}
