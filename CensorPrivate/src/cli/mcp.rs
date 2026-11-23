use censorprivacy::mcp::{HttpPrivateComputeClient, StdioBridge};

#[tokio::main]
pub async fn run() {
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
