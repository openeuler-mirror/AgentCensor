use censorprivacy::{
    json,
    mcp::{validate_arguments, HttpPrivateComputeClient, PrivateComputeClient},
};
use clap::Parser;

#[derive(Parser)]
#[command(name = "privcalc", about = "Compute locally using private references")]
struct Cli {
    #[arg(value_parser=["sum","mean","min","max","aggregate","date_diff"])]
    operation: String,
    #[arg(required = true)]
    refs: Vec<String>,
    #[arg(long, default_value = "{}")]
    options: String,
}
#[tokio::main]
async fn main() {
    std::panic::set_hook(Box::new(|_| {
        eprintln!("{{\"error\":{{\"code\":\"private_compute_failed\"}}}}")
    }));
    let cli = Cli::parse();
    let result = async {
        let client = HttpPrivateComputeClient::from_environment()?;
        let options = json::from_slice(cli.options.as_bytes())?;
        let aliases = std::env::var("CENSORPRIVACY_MODE").as_deref() == Ok("tls-direct");
        let arguments = validate_arguments(
            &serde_json::json!({"operation":cli.operation,"refs":cli.refs,"options":options}),
            aliases,
        )?;
        client.compute(arguments).await
    }
    .await;
    match result {
        Ok(value) => println!("{value}"),
        Err(_) => {
            eprintln!("{{\"error\":{{\"code\":\"private_compute_failed\"}}}}");
            std::process::exit(2);
        }
    }
}
