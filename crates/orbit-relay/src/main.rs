use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(name = "orbit-relay", about = "Orbit-Transfer edge relay server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the edge relay server
    Serve {
        #[arg(long, default_value = "0.0.0.0:9000")]
        addr: String,
        /// Cap egress per connection (kbps) — simulate a constrained uplink
        #[arg(long)]
        throttle_kbps: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            addr,
            throttle_kbps,
        } => {
            let listener = TcpListener::bind(&addr).await?;
            let core = Arc::new(orbit_relay::RelayCore::with_throttle(throttle_kbps));
            core.run(listener).await?;
        }
    }
    Ok(())
}