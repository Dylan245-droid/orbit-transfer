use clap::{Parser, Subcommand};
use orbit_transport::{ReceiverSession, SenderSession};
use std::fs;
use std::path::PathBuf;
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(
    name = "orbit",
    about = "Orbit-Transfer: hybrid P2P + edge relay file transfer with fountain codes"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send a file through one or more relays to a receiver
    Send {
        /// Path to the file to send
        file: PathBuf,
        /// Relay WebSocket URL (e.g. ws://127.0.0.1:9000); repeatable to
        /// aggregate bandwidth over several relays
        #[arg(long = "relay", action = clap::ArgAction::Append, default_value = "ws://127.0.0.1:9000")]
        relay: Vec<String>,
        /// Fountain symbol size in bytes
        #[arg(long, default_value_t = 65536)]
        symbol_size: usize,
        /// Session id (generated randomly if omitted)
        #[arg(long)]
        session: Option<u64>,
        /// Optional passphrase sealing every symbol (AEAD)
        #[arg(long)]
        secret: Option<String>,
    },
    /// Receive a file from a sender through one or more relays
    Receive {
        /// Path where the received file is written
        output: PathBuf,
        /// Relay WebSocket URL (e.g. ws://127.0.0.1:9000); repeatable to
        /// aggregate bandwidth over several relays
        #[arg(long = "relay", action = clap::ArgAction::Append, default_value = "ws://127.0.0.1:9000")]
        relay: Vec<String>,
        /// Session id to join
        #[arg(long)]
        session: u64,
        /// Optional passphrase to open sealed symbols (AEAD)
        #[arg(long)]
        secret: Option<String>,
        /// P2P listen address advertised to the sender (default: ephemeral)
        #[arg(long)]
        listen: Option<String>,
        /// Use QUIC (instead of TCP) for the direct P2P path
        #[arg(long)]
        quic: bool,
        /// Disable the P2P direct path entirely (relay-only transfer)
        #[arg(long)]
        no_p2p: bool,
    },
    /// Run a relay server
    Relay {
        #[arg(long, default_value = "0.0.0.0:9000")]
        addr: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Relay { addr } => {
            let listener = TcpListener::bind(&addr).await?;
            println!("orbit-relay listening on {addr}");
            let core = std::sync::Arc::new(orbit_relay::RelayCore::new());
            core.run(listener).await?;
        }
        Command::Send {
            file,
            relay,
            symbol_size,
            session,
            secret,
        } => {
            let payload = fs::read(&file)?;
            let name = file
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "payload.bin".to_string());
            let session_id = session.unwrap_or_else(rand::random);
            println!(
                "sending '{name}' ({:.2} MiB) on session {session_id} via [{}]{}",
                payload.len() as f64 / 1048576.0,
                relay.join(", "),
                if secret.is_some() { " (encrypted)" } else { "" }
            );

            let mut sender =
                SenderSession::connect(&relay, session_id, name, payload, symbol_size, secret)
                    .await?;
            let stats = sender.run().await?;
            println!("transfer complete in {:.3}s", stats.elapsed.as_secs_f64());
            println!(
                "  payload:      {:.2} MiB",
                stats.bytes as f64 / 1048576.0
            );
            println!("  symbols:      {} (overhead x{:.3})", stats.symbols, stats.overhead_ratio);
            println!("  paths:        {} direct / {} relay", stats.via_direct, stats.via_relay);
            println!("  throughput:   {:.2} MiB/s", stats.bytes as f64 / 1048576.0 / stats.elapsed.as_secs_f64());
        }
        Command::Receive {
            output,
            relay,
            session,
            secret,
            listen,
            quic,
            no_p2p,
        } => {
            println!(
                "receiving session {session} via [{}]{}{}",
                relay.join(", "),
                if secret.is_some() { " (encrypted)" } else { "" },
                if quic { " (QUIC direct path)" } else { "" }
            );
            let listen_addr = if no_p2p {
                None
            } else {
                match (listen, quic) {
                    (Some(addr), true) => Some(format!("quic://{addr}")),
                    (Some(addr), false) => Some(addr),
                    (None, true) => Some("quic://127.0.0.1:0".to_string()),
                    (None, false) => Some("127.0.0.1:0".to_string()),
                }
            };
            let mut receiver =
                ReceiverSession::connect(&relay, session, listen_addr, secret).await?;
            let (data, stats) = receiver.run().await?;
            fs::write(&output, &data)?;
            println!(
                "received '{}' ({:.2} MiB) in {:.3}s",
                receiver.filename().unwrap_or("payload.bin"),
                data.len() as f64 / 1048576.0,
                stats.elapsed.as_secs_f64()
            );
            println!(
                "  symbols:      {} (overhead x{:.3})",
                stats.symbols, stats.overhead_ratio
            );
            println!("  paths:        {} direct / {} relay", stats.via_direct, stats.via_relay);
            println!(
                "  throughput:   {:.2} MiB/s",
                data.len() as f64 / 1048576.0 / stats.elapsed.as_secs_f64()
            );
            println!("  written to:   {}", output.display());
        }
    }
    Ok(())
}