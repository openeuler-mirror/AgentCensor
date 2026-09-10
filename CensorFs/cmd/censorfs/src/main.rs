use clap::{ArgGroup, Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use censorfs_core::control::*;
use censorfs_core::fsck::{run_fsck, FsckReport};
use censorfs_core::*;

type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Parser)]
#[command(name = "censorfs", about = "Readable CensorFS demonstration commands")]
struct Arguments {
    #[arg(long, global = true)]
    storage_root: Option<PathBuf>,
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[arg(long, global = true)]
    request_id: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init {
        #[arg(long)]
        import_root: PathBuf,
        #[arg(long, default_value = "main")]
        branch: String,
    },
    Daemon,
    Info,
    BranchCreate {
        name: String,
        #[arg(long)]
        from: String,
    },
    BranchList,
    Head {
        branch: String,
        #[arg(long)]
        generation_only: bool,
    },
    Explore {
        branch: String,
        #[arg(long)]
        id_only: bool,
    },
    Ls {
        #[command(flatten)]
        selector: SelectorArgs,
        path: String,
    },
    Cat {
        #[command(flatten)]
        selector: SelectorArgs,
        path: String,
    },
    Write {
        #[arg(long)]
        ticket: String,
        path: String,
        #[command(flatten)]
        input: WriteInput,
    },
    Mkdir {
        #[arg(long)]
        ticket: String,
        path: String,
    },
    Rm {
        #[arg(long)]
        ticket: String,
        path: String,
    },
    Mv {
        #[arg(long)]
        ticket: String,
        source: String,
        target: String,
    },
    Commit {
        ticket: String,
        #[arg(long, default_value = "censorfs commit")]
        message: String,
        #[arg(long)]
        generation_only: bool,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
    },
    Abort {
        ticket: String,
    },
    Diff {
        left_generation: String,
        right_generation: String,
    },
    DiffText {
        left_generation: String,
        right_generation: String,
        #[arg(long, default_value_t = 256 * 1024)]
        max_file_bytes: u64,
    },
    VariantOpen {
        #[arg(long)]
        branch: String,
        #[arg(long)]
        expected_generation: String,
        #[arg(long)]
        expected_head_seq: u64,
        #[arg(long)]
        run: String,
        #[arg(long)]
        variant: String,
    },
    VariantPrepare {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        view: Option<String>,
        #[arg(long)]
        run: String,
        #[arg(long)]
        variant: String,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = 256 * 1024)]
        max_diff_file_bytes: u64,
    },
    VariantPublish {
        #[arg(long)]
        candidate: String,
        #[arg(long)]
        expected_generation: String,
        #[arg(long)]
        expected_head_seq: u64,
        #[arg(long)]
        decision_id: String,
        #[arg(long)]
        run: String,
        #[arg(long)]
        variant: String,
    },
    VariantAbort {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        view: Option<String>,
        #[arg(long)]
        run: String,
        #[arg(long)]
        variant: String,
    },
    CandidateViewOpen {
        #[arg(long)]
        candidate: String,
    },
    GenerationViewOpen {
        #[arg(long)]
        generation: String,
    },
    ViewClose {
        #[arg(long)]
        view: String,
    },
    Merge {
        source: String,
        #[arg(long)]
        into: String,
        #[arg(long)]
        check: bool,
        #[arg(long, default_value = "censorfs merge")]
        message: String,
    },
    Generation {
        generation: String,
    },
    Fsck {
        #[arg(long)]
        repair: bool,
    },
}

impl Command {
    fn machine_json(&self) -> bool {
        matches!(
            self,
            Self::DiffText { .. }
                | Self::VariantOpen { .. }
                | Self::VariantPrepare { .. }
                | Self::VariantPublish { .. }
                | Self::VariantAbort { .. }
                | Self::CandidateViewOpen { .. }
                | Self::GenerationViewOpen { .. }
                | Self::ViewClose { .. }
        )
    }
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("selector")
        .required(true)
        .multiple(false)
        .args(["branch", "ticket", "generation", "candidate"])
))]
struct SelectorArgs {
    #[arg(long)]
    branch: Option<String>,
    #[arg(long)]
    ticket: Option<String>,
    #[arg(long)]
    generation: Option<String>,
    #[arg(long)]
    candidate: Option<String>,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("input")
        .required(true)
        .multiple(false)
        .args(["text", "from", "stdin"])
))]
struct WriteInput {
    #[arg(long)]
    text: Option<String>,
    #[arg(long)]
    from: Option<PathBuf>,
    #[arg(long)]
    stdin: bool,
}

fn main() {
    let arguments = Arguments::parse_from(normalize_multicall(std::env::args_os().collect()));
    let machine_json = arguments.command.machine_json();
    match run(arguments) {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if machine_json {
                let code = error
                    .downcast_ref::<CensorFsError>()
                    .map(|value| format!("{:?}", value.code))
                    .unwrap_or_else(|| "Error".to_string());
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "ok": false,
                        "code": code,
                        "error": error.to_string(),
                    }))
                    .unwrap()
                );
            } else {
                eprintln!("censorfs: {error}");
            }
            std::process::exit(1);
        }
    }
}

fn run(arguments: Arguments) -> AnyResult<i32> {
    let storage_root = arguments
        .storage_root
        .or_else(|| std::env::var_os("CENSORFS_STORAGE_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(".censorfs"));
    let socket = arguments
        .socket
        .or_else(|| std::env::var_os("CENSORFS_SOCKET").map(PathBuf::from))
        .unwrap_or_else(|| {
            storage_root
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("control.sock")
        });
    let request_id = match arguments.request_id {
        Some(value) => RequestId::parse(&value)?,
        None => RequestId::new(),
    };
    let json_output = arguments.json;

    match arguments.command {
        Command::Init {
            import_root,
            branch,
        } => {
            let fs = CensorFs::initialize(InitOptions {
                storage_root,
                import_root,
                main_branch: branch_id(branch)?,
            })?;
            output_metadata(&fs.recovery_report(), json_output)?;
        }
        Command::Daemon => return run_daemon(storage_root, socket),
        Command::Fsck { repair } => {
            let report = match run_fsck(storage_root, repair) {
                Ok(report) => report,
                Err(error) => {
                    eprintln!("censorfs-fsck: {error}");
                    return Ok(2);
                }
            };
            print_fsck(&report, json_output)?;
            return Ok(if report.clean { 0 } else { 1 });
        }
        command => {
            let client = ControlClient::new(socket);
            return run_online(command, &client, request_id, json_output);
        }
    }
    Ok(0)
}

fn run_online(
    command: Command,
    client: &ControlClient,
    request_id: RequestId,
    json_output: bool,
) -> AnyResult<i32> {
    match command {
        Command::Info => {
            let info: FsInfo = client.call(Operation::GetFsInfo, request_id, &())?;
            output_metadata(&info, json_output)?;
        }
        Command::BranchCreate { name, from } => {
            let head: BranchHead = client.call(
                Operation::CreateBranchFrom,
                request_id,
                &CreateBranchFromRequest {
                    branch_id: branch_id(name)?,
                    from_branch: branch_id(from)?,
                },
            )?;
            output_metadata(&head, json_output)?;
        }
        Command::BranchList => {
            let branches: Vec<BranchInfo> =
                client.call(Operation::ListBranches, request_id, &())?;
            if json_output {
                print_json(&branches)?;
            } else {
                println!("BRANCH\tGENERATION\tHEAD_SEQ");
                for branch in branches {
                    println!(
                        "{}\t{}\t{}",
                        branch.branch_id, branch.head.generation_id, branch.head.head_seq
                    );
                }
            }
        }
        Command::Head {
            branch,
            generation_only,
        } => {
            let head: BranchHead = client.call(
                Operation::GetBranchHead,
                request_id,
                &BranchRequest {
                    branch_id: branch_id(branch)?,
                },
            )?;
            if generation_only {
                println!("{}", head.generation_id);
            } else if json_output {
                print_json(&head)?;
            } else {
                println!("generation: {}", head.generation_id);
                println!("head-seq: {}", head.head_seq);
            }
        }
        Command::Explore { branch, id_only } => {
            let result: ExplorationResult = client.call(
                Operation::Explore,
                request_id,
                &ExploreRequest {
                    branch_id: branch_id(branch)?,
                },
            )?;
            if id_only {
                println!("{}", result.ticket.ticket_id);
            } else if json_output {
                print_json(&result)?;
            } else {
                println!("ticket: {}", result.ticket.ticket_id);
                println!("transaction: {}", result.tx.tx_id);
                println!("branch: {}", result.ticket.branch_id);
                println!("base-generation: {}", result.ticket.base_generation);
            }
        }
        Command::Ls { selector, path } => {
            let entries: Vec<ManifestEntry> = client.call(
                Operation::TestList,
                request_id,
                &ViewPathRequest {
                    selector: selector.parse()?,
                    path: logical_path(&path)?,
                },
            )?;
            if json_output {
                let entries: Vec<_> = entries.iter().map(entry_json).collect();
                print_json(&entries)?;
            } else {
                println!("TYPE\tSIZE\tPATH");
                for entry in entries {
                    println!(
                        "{}\t{}\t/{}",
                        if entry.kind == EntryKind::Directory {
                            "dir"
                        } else {
                            "file"
                        },
                        entry.size,
                        entry.path
                    );
                }
            }
        }
        Command::Cat { selector, path } => {
            let contents: Vec<u8> = client.call(
                Operation::TestCat,
                request_id,
                &ViewPathRequest {
                    selector: selector.parse()?,
                    path: logical_path(&path)?,
                },
            )?;
            std::io::stdout().write_all(&contents)?;
        }
        Command::Write {
            ticket,
            path,
            input,
        } => {
            let data = input.read()?;
            if data.len() > MAX_FILE_CONTENT {
                return Err(format!("input is {} bytes; maximum is 1 MiB", data.len()).into());
            }
            let _: () = client.call(
                Operation::TestWrite,
                request_id,
                &TicketWriteRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                    path: logical_path(&path)?,
                    data,
                },
            )?;
            output_ok(json_output)?;
        }
        Command::Mkdir { ticket, path } => {
            let _: () = client.call(
                Operation::TestMkdir,
                request_id,
                &TicketPathRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                    path: logical_path(&path)?,
                },
            )?;
            output_ok(json_output)?;
        }
        Command::Rm { ticket, path } => {
            let _: () = client.call(
                Operation::TestRemove,
                request_id,
                &TicketPathRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                    path: logical_path(&path)?,
                },
            )?;
            output_ok(json_output)?;
        }
        Command::Mv {
            ticket,
            source,
            target,
        } => {
            let _: () = client.call(
                Operation::TestMove,
                request_id,
                &TicketMoveRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                    source: logical_path(&source)?,
                    target: logical_path(&target)?,
                },
            )?;
            output_ok(json_output)?;
        }
        Command::Commit {
            ticket,
            message,
            generation_only,
            timeout_ms,
        } => {
            let result: CommitResult = client.call(
                Operation::CommitExploration,
                request_id,
                &CommitExplorationRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                    message,
                    timeout_ms,
                },
            )?;
            if generation_only {
                println!("{}", result.generation.generation_id);
            } else if json_output {
                print_json(&result)?;
            } else {
                println!("published-generation: {}", result.generation.generation_id);
                println!("branch: {}", result.receipt.branch_id);
                println!("head-seq: {}", result.receipt.head_seq);
                println!("candidate: {}", result.candidate.candidate_id);
            }
        }
        Command::Abort { ticket } => {
            let result: AbortResult = client.call(
                Operation::AbortExploration,
                request_id,
                &TicketRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                },
            )?;
            output_metadata(&result, json_output)?;
        }
        Command::Diff {
            left_generation,
            right_generation,
        } => {
            let diffs: Vec<PathDiff> = client.call(
                Operation::DiffGenerations,
                request_id,
                &DiffRequest {
                    left: GenerationId::parse(&left_generation)?,
                    right: GenerationId::parse(&right_generation)?,
                },
            )?;
            if json_output {
                let values: Vec<_> = diffs
                    .iter()
                    .map(|diff| json!({"path": format!("/{}", diff.path), "kind": format!("{:?}", diff.kind)}))
                    .collect();
                print_json(&values)?;
            } else if diffs.is_empty() {
                println!("no differences");
            } else {
                for diff in diffs {
                    println!("{:?}\t/{}", diff.kind, diff.path);
                }
            }
        }
        Command::DiffText {
            left_generation,
            right_generation,
            max_file_bytes,
        } => {
            let report: TextDiffReport = client.call(
                Operation::DiffText,
                request_id,
                &TextDiffRequest {
                    left: GenerationId::parse(&left_generation)?,
                    right: GenerationId::parse(&right_generation)?,
                    max_file_bytes,
                },
            )?;
            print_json(&json!({"ok": true, "result": text_diff_json(&report)}))?;
        }
        Command::VariantOpen {
            branch,
            expected_generation,
            expected_head_seq,
            run,
            variant,
        } => {
            let result: VariantOpenResult = client.call(
                Operation::VariantOpen,
                request_id,
                &VariantOpenRequest {
                    branch_id: branch_id(branch)?,
                    expected_head: BranchHead {
                        generation_id: GenerationId::parse(&expected_generation)?,
                        head_seq: expected_head_seq,
                    },
                    run_id: run,
                    variant_id: variant,
                },
            )?;
            print_json(&json!({"ok": true, "result": result}))?;
        }
        Command::VariantPrepare {
            ticket,
            view,
            run,
            variant,
            timeout_ms,
            max_diff_file_bytes,
        } => {
            let result: VariantPrepareResult = client.call(
                Operation::VariantPrepare,
                request_id,
                &VariantPrepareRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                    view_id: view.as_deref().map(ViewId::parse).transpose()?,
                    run_id: run,
                    variant_id: variant,
                    timeout_ms,
                    max_diff_file_bytes,
                },
            )?;
            print_json(&json!({
                "ok": true,
                "result": {
                    "run_id": result.run_id,
                    "variant_id": result.variant_id,
                    "ticket": result.ticket,
                    "candidate": result.candidate,
                    "generation": result.generation,
                    "path_diff": result.path_diff.iter().map(path_diff_json).collect::<Vec<_>>(),
                    "text_diff": text_diff_json(&result.text_diff),
                }
            }))?;
        }
        Command::VariantPublish {
            candidate,
            expected_generation,
            expected_head_seq,
            decision_id,
            run,
            variant,
        } => {
            let result: VariantPublishResult = client.call(
                Operation::VariantPublish,
                request_id,
                &VariantPublishRequest {
                    candidate_id: CandidateId::parse(&candidate)?,
                    expected_head: BranchHead {
                        generation_id: GenerationId::parse(&expected_generation)?,
                        head_seq: expected_head_seq,
                    },
                    decision_id,
                    run_id: run,
                    variant_id: variant,
                },
            )?;
            print_json(&json!({"ok": true, "result": result}))?;
        }
        Command::VariantAbort {
            ticket,
            view,
            run,
            variant,
        } => {
            let result: VariantAbortResult = client.call(
                Operation::VariantAbort,
                request_id,
                &VariantAbortRequest {
                    ticket_id: TicketId::parse(&ticket)?,
                    view_id: view.as_deref().map(ViewId::parse).transpose()?,
                    run_id: run,
                    variant_id: variant,
                },
            )?;
            print_json(&json!({"ok": true, "result": result}))?;
        }
        Command::CandidateViewOpen { candidate } => {
            let result: ViewHandle = client.call(
                Operation::OpenView,
                request_id,
                &OpenViewRequest {
                    selector: ViewSelector::Candidate(CandidateId::parse(&candidate)?),
                },
            )?;
            print_json(&json!({"ok": true, "result": result}))?;
        }
        Command::GenerationViewOpen { generation } => {
            let result: ViewHandle = client.call(
                Operation::OpenView,
                request_id,
                &OpenViewRequest {
                    selector: ViewSelector::Generation(GenerationId::parse(&generation)?),
                },
            )?;
            print_json(&json!({"ok": true, "result": result}))?;
        }
        Command::ViewClose { view } => {
            let result: ViewHandle = client.call(
                Operation::CloseView,
                request_id,
                &CloseViewRequest {
                    view_id: ViewId::parse(&view)?,
                },
            )?;
            print_json(&json!({"ok": true, "result": result}))?;
        }
        Command::Merge {
            source,
            into,
            check,
            message,
        } => {
            let result: MergeResult = client.call(
                Operation::MergeBranches,
                request_id,
                &MergeBranchesRequest {
                    source_branch: branch_id(source)?,
                    target_branch: branch_id(into)?,
                    message,
                    check_only: check,
                },
            )?;
            if json_output {
                print_json(&merge_json(&result))?;
            } else if result.check.already_up_to_date {
                println!("already up to date");
            } else if !result.check.conflicts.is_empty() {
                println!("merge conflicts:");
                for conflict in &result.check.conflicts {
                    println!("{:?}\t/{}", conflict.reason, conflict.path);
                }
            } else if check {
                println!("merge check passed: no conflicts");
            } else if let Some(receipt) = &result.receipt {
                println!("merged-generation: {}", receipt.new_generation);
                println!("target: {}", receipt.branch_id);
                println!("head-seq: {}", receipt.head_seq);
            }
            if !result.check.conflicts.is_empty() {
                return Ok(1);
            }
        }
        Command::Generation { generation } => {
            let value: GenerationMeta = client.call(
                Operation::GetGenerationInfo,
                request_id,
                &GenerationRequest {
                    generation_id: GenerationId::parse(&generation)?,
                },
            )?;
            if json_output {
                print_json(&value)?;
            } else {
                println!("generation: {}", value.generation_id);
                println!("kind: {:?}", value.kind);
                println!(
                    "parents: {}",
                    value
                        .parents
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!("entries: {}", value.entry_count);
            }
        }
        Command::Init { .. } | Command::Daemon | Command::Fsck { .. } => unreachable!(),
    }
    Ok(0)
}

impl SelectorArgs {
    fn parse(self) -> AnyResult<ViewSelector> {
        if let Some(branch) = self.branch {
            Ok(ViewSelector::Branch(branch_id(branch)?))
        } else if let Some(ticket) = self.ticket {
            Ok(ViewSelector::Ticket(TicketId::parse(&ticket)?))
        } else if let Some(generation) = self.generation {
            Ok(ViewSelector::Generation(GenerationId::parse(&generation)?))
        } else if let Some(candidate) = self.candidate {
            Ok(ViewSelector::Candidate(CandidateId::parse(&candidate)?))
        } else {
            unreachable!("clap requires exactly one selector")
        }
    }
}

impl WriteInput {
    fn read(self) -> AnyResult<Vec<u8>> {
        if let Some(text) = self.text {
            return Ok(text.into_bytes());
        }
        if let Some(path) = self.from {
            return Ok(std::fs::read(path)?);
        }
        let mut data = Vec::new();
        std::io::stdin()
            .take((MAX_FILE_CONTENT + 1) as u64)
            .read_to_end(&mut data)?;
        Ok(data)
    }
}

fn normalize_multicall(mut args: Vec<OsString>) -> Vec<OsString> {
    let invoked = args
        .first()
        .and_then(|value| Path::new(value).file_stem())
        .and_then(|value| value.to_str())
        .unwrap_or("censorfs");
    if let Some(command) = invoked.strip_prefix("censorfs-") {
        if !command.is_empty() {
            args.insert(1, OsString::from(command));
        }
    }
    args
}

fn logical_path(value: &str) -> AnyResult<LogicalPath> {
    let value = value.trim();
    let normalized = if value == "/" || value == "." {
        ""
    } else {
        value.strip_prefix('/').unwrap_or(value)
    };
    Ok(LogicalPath::from_utf8(normalized)?)
}

fn branch_id(value: String) -> AnyResult<BranchId> {
    BranchId::new(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error).into())
}

fn output_metadata<T: Serialize + std::fmt::Debug>(value: &T, json_output: bool) -> AnyResult<()> {
    if json_output {
        print_json(value)
    } else {
        println!("{value:#?}");
        Ok(())
    }
}

fn output_ok(json_output: bool) -> AnyResult<()> {
    if json_output {
        print_json(&json!({"ok": true}))
    } else {
        println!("ok");
        Ok(())
    }
}

fn print_json<T: Serialize>(value: &T) -> AnyResult<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn entry_json(entry: &ManifestEntry) -> serde_json::Value {
    json!({
        "path": format!("/{}", entry.path),
        "kind": format!("{:?}", entry.kind),
        "mode": format!("{:o}", entry.mode),
        "size": entry.size,
        "object_id": entry.object_id.map(|value| value.to_string()),
    })
}

fn path_diff_json(diff: &PathDiff) -> serde_json::Value {
    json!({
        "path": format!("/{}", diff.path),
        "kind": format!("{:?}", diff.kind),
    })
}

fn text_diff_json(report: &TextDiffReport) -> serde_json::Value {
    json!({
        "left": report.left.to_string(),
        "right": report.right.to_string(),
        "files": report.files.iter().map(|file| json!({
            "path": format!("/{}", file.path),
            "kind": format!("{:?}", file.kind),
            "disposition": format!("{:?}", file.disposition),
            "old_size": file.old_size,
            "new_size": file.new_size,
            "old_digest": file.old_digest,
            "new_digest": file.new_digest,
            "patch": file.patch,
        })).collect::<Vec<_>>(),
    })
}

fn merge_json(result: &MergeResult) -> serde_json::Value {
    json!({
        "source_branch": result.check.source_branch.to_string(),
        "target_branch": result.check.target_branch.to_string(),
        "source_head": result.check.source_head,
        "target_head": result.check.target_head,
        "merge_base": result.check.merge_base.to_string(),
        "already_up_to_date": result.check.already_up_to_date,
        "conflicts": result.check.conflicts.iter().map(|conflict| json!({
            "path": format!("/{}", conflict.path),
            "reason": format!("{:?}", conflict.reason),
        })).collect::<Vec<_>>(),
        "candidate": result.prepared.as_ref().map(|value| value.candidate.candidate_id.to_string()),
        "generation": result.receipt.as_ref().map(|value| value.new_generation.to_string()),
        "receipt": result.receipt,
    })
}

fn print_fsck(report: &FsckReport, json_output: bool) -> AnyResult<()> {
    if json_output {
        return print_json(report);
    }
    println!(
        "CensorFS fsck: {}",
        if report.clean {
            "clean"
        } else {
            "issues found"
        }
    );
    for repair in &report.repairs {
        println!("REPAIRED\t{repair}");
    }
    for issue in &report.issues {
        println!(
            "{}\t{}\t{}",
            if issue.repairable {
                "REPAIRABLE"
            } else {
                "ERROR"
            },
            issue.code,
            issue.message
        );
    }
    println!(
        "checked: {} branches, {} generations, {} objects, {} tickets, {} candidates",
        report.stats.branches,
        report.stats.generations,
        report.stats.objects,
        report.stats.tickets,
        report.stats.candidates
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_daemon(storage_root: PathBuf, socket: PathBuf) -> AnyResult<i32> {
    let fs = CensorFs::open(storage_root)?;
    ControlServer::new(socket, fs).run()?;
    Ok(0)
}

#[cfg(not(target_os = "linux"))]
fn run_daemon(_storage_root: PathBuf, _socket: PathBuf) -> AnyResult<i32> {
    Err("censorfs-daemon requires Linux".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multicall_and_subcommand_parse_the_same_command() {
        let direct = Arguments::try_parse_from(["censorfs", "head", "main"]).unwrap();
        let alias = Arguments::try_parse_from(normalize_multicall(vec![
            "/usr/bin/censorfs-head".into(),
            "main".into(),
        ]))
        .unwrap();
        assert!(matches!(direct.command, Command::Head { branch, .. } if branch == "main"));
        assert!(matches!(alias.command, Command::Head { branch, .. } if branch == "main"));
    }

    #[test]
    fn selector_is_exclusive() {
        assert!(Arguments::try_parse_from([
            "censorfs",
            "ls",
            "--branch",
            "main",
            "--ticket",
            "00000000-0000-0000-0000-000000000000",
            "/"
        ])
        .is_err());
    }

    #[test]
    fn harness_machine_commands_use_stable_kebab_case_names() {
        let generation = "00000000-0000-0000-0000-000000000001";
        let candidate = "00000000-0000-0000-0000-000000000002";
        let ticket = "00000000-0000-0000-0000-000000000003";
        let view = "00000000-0000-0000-0000-000000000004";
        for args in [
            vec![
                "censorfs",
                "variant-open",
                "--branch",
                "main",
                "--expected-generation",
                generation,
                "--expected-head-seq",
                "1",
                "--run",
                "run-1",
                "--variant",
                "minimal",
            ],
            vec![
                "censorfs",
                "variant-prepare",
                "--ticket",
                ticket,
                "--view",
                view,
                "--run",
                "run-1",
                "--variant",
                "minimal",
            ],
            vec![
                "censorfs",
                "variant-publish",
                "--candidate",
                candidate,
                "--expected-generation",
                generation,
                "--expected-head-seq",
                "1",
                "--decision-id",
                "user-choice",
                "--run",
                "run-1",
                "--variant",
                "minimal",
            ],
            vec![
                "censorfs",
                "variant-abort",
                "--ticket",
                ticket,
                "--run",
                "run-1",
                "--variant",
                "minimal",
            ],
            vec!["censorfs", "candidate-view-open", "--candidate", candidate],
            vec!["censorfs", "view-close", "--view", view],
        ] {
            let parsed = Arguments::try_parse_from(args).unwrap();
            assert!(parsed.command.machine_json());
        }
    }
}
