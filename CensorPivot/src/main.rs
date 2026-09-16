use censorpivot::adapters::{CommandAdapters, run_local_call};
use censorpivot::config::Config;
use censorpivot::model::{
    BatchRequest, ClientReply, ClientRequest, RunnerReply, RunnerRequest, TransactionState,
};
use censorpivot::server;
use censorpivot::store::TransactionStore;
use censorpivot::{Engine, PivotError, Result};
use clap::{Parser, Subcommand};
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const DEFAULT_CONFIG: &str = "/etc/censorpivot/config.json";
const DEFAULT_SOCKET: &str = "/run/censorpivot/control.sock";
const MAX_CLIENT_REPLY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "censorpivot",
    version,
    about = "AgentCensor ingress and transaction coordinator"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Serve {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
    Submit {
        #[arg(long, default_value = DEFAULT_SOCKET)]
        socket: PathBuf,
        #[arg(long, default_value = "-")]
        file: String,
    },
    Status {
        transaction_id: Uuid,
        #[arg(long, default_value = DEFAULT_SOCKET)]
        socket: PathBuf,
    },
    Recover {
        #[arg(long, default_value = DEFAULT_SOCKET)]
        socket: PathBuf,
    },
    Doctor {
        #[arg(long, default_value = DEFAULT_SOCKET)]
        socket: PathBuf,
    },
    #[command(name = "__batch-runner", hide = true)]
    BatchRunner {
        #[arg(long)]
        max_output_bytes: usize,
        #[arg(long, default_value_t = 1048576)]
        max_frame_bytes: usize,
    },
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("censorpivot: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32> {
    match Cli::parse().command {
        Commands::Serve { config } => serve(&config).map(|()| 0),
        Commands::Submit { socket, file } => {
            let batch: BatchRequest = serde_json::from_slice(&read_input(&file)?)?;
            print_reply(server::request(
                &socket,
                &ClientRequest::Execute { batch },
                MAX_CLIENT_REPLY_BYTES,
            )?)
        }
        Commands::Status {
            transaction_id,
            socket,
        } => print_reply(server::request(
            &socket,
            &ClientRequest::Status { transaction_id },
            MAX_CLIENT_REPLY_BYTES,
        )?),
        Commands::Recover { socket } => print_reply(server::request(
            &socket,
            &ClientRequest::Recover,
            MAX_CLIENT_REPLY_BYTES,
        )?),
        Commands::Doctor { socket } => print_reply(server::request(
            &socket,
            &ClientRequest::Doctor,
            MAX_CLIENT_REPLY_BYTES,
        )?),
        Commands::BatchRunner {
            max_output_bytes,
            max_frame_bytes,
        } => batch_runner(max_output_bytes, max_frame_bytes),
    }
}

fn serve(config_path: &Path) -> Result<()> {
    if nix::unistd::geteuid().is_root() {
        return Err(PivotError::Invalid(
            "serve requires a dedicated non-root UID (View owner becomes runner UID)".into(),
        ));
    }
    let config = Config::load(config_path)?;
    let store = TransactionStore::open(&config.state_dir)?;
    let lock_path = config.state_dir.join(".coordinator.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock.try_lock_exclusive().map_err(|error| {
        PivotError::Conflict(format!(
            "state directory {} is already owned: {error}",
            config.state_dir.display()
        ))
    })?;
    let adapters = CommandAdapters::new(config.clone());
    let engine = Engine::new(
        store,
        adapters.clone(),
        adapters.clone(),
        adapters.clone(),
        config.max_calls_per_batch,
    );
    let recovered = engine.recover(None)?;
    let pending = recovered
        .iter()
        .filter(|record| !record.state.terminal())
        .count();
    if pending > 0 {
        eprintln!("censorpivot: {pending} decided transactions still require recovery");
    }
    server::serve(&engine, &adapters, &config)?;
    drop(lock);
    Ok(())
}

fn read_input(file: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let limit = 16 * 1024 * 1024;
    if file == "-" {
        io::stdin().take(limit + 1).read_to_end(&mut bytes)?;
    } else {
        fs::File::open(file)?
            .take(limit + 1)
            .read_to_end(&mut bytes)?;
    }
    if bytes.len() as u64 > limit {
        return Err(PivotError::Invalid("input exceeds 16 MiB".into()));
    }
    Ok(bytes)
}

fn print_reply(reply: ClientReply) -> Result<i32> {
    println!("{}", serde_json::to_string_pretty(&reply)?);
    Ok(match reply {
        ClientReply::Transaction(record) => match record.state {
            TransactionState::Committed => 0,
            TransactionState::Aborted => 2,
            _ => 3,
        },
        ClientReply::Transactions(records) => {
            if records.iter().all(|record| record.state.terminal()) {
                0
            } else {
                3
            }
        }
        ClientReply::Doctor(report) => i32::from(!report.ready),
        ClientReply::Error { .. } => 1,
    })
}

fn batch_runner(max_output_bytes: usize, max_frame_bytes: usize) -> Result<i32> {
    if max_output_bytes == 0
        || max_output_bytes > 1024 * 1024
        || max_frame_bytes == 0
        || max_frame_bytes > 16 * 1024 * 1024
    {
        return Err(PivotError::Invalid(
            "max_output_bytes must be greater than zero".into(),
        ));
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(
        &mut output,
        &RunnerReply::Ready {
            host_pid: std::process::id(),
        },
    )?;
    output.write_all(b"\n")?;
    output.flush()?;
    let mut line = String::new();
    loop {
        line.clear();
        if (&mut input)
            .take(max_frame_bytes as u64 + 1)
            .read_line(&mut line)?
            == 0
        {
            return Ok(0);
        }
        if line.len() > max_frame_bytes || !line.ends_with('\n') {
            return Err(PivotError::Protocol(
                "runner request is oversized or incomplete".into(),
            ));
        }
        let reply = match serde_json::from_str::<RunnerRequest>(&line) {
            Ok(request) => match run_local_call(&request, max_output_bytes) {
                Ok(result) => RunnerReply::Result(result),
                Err(error) => RunnerReply::Error(error.to_string()),
            },
            Err(error) => RunnerReply::Error(format!("invalid runner request: {error}")),
        };
        serde_json::to_writer(&mut output, &reply)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}
