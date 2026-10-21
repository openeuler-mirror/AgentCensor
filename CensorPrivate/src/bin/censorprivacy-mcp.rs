use censorprivacy::mcp::{HttpPrivateComputeClient, StdioBridge};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "censorprivacy-mcp",
    version,
    about = "Private computation MCP server over stdio; configure CENSORPRIVACY_URL, CENSORPRIVACY_TOKEN and CENSORPRIVACY_RUN_ID"
)]
struct Cli {}

#[tokio::main]
async fn main() {
    let _ = Cli::parse();
    std::panic::set_hook(Box::new(|_| eprintln!("CensorPrivacy MCP unavailable")));
    let client = match HttpPrivateComputeClient::from_environment() {
        Ok(client) => client,
        Err(_) => {
            eprintln!("CensorPrivacy MCP configuration unavailable");
            std::process::exit(2);
        }
    };
    let aliases = std::env::var("CENSORPRIVACY_MODE").as_deref() == Ok("tls-direct");
    if StdioBridge::new(client, aliases)
        .run(std::io::stdin().lock(), std::io::stdout().lock())
        .await
        .is_err()
    {
        eprintln!("CensorPrivacy MCP protocol unavailable");
        std::process::exit(2);
    }
}
