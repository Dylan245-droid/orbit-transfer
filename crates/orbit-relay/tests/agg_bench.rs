//! Bandwidth-aggregation bench: N throttled relays vs a single relay.
//!
//! Each relay caps egress at `KBPS`, so one relay is the bottleneck;
//! N relays must aggregate ~N× the throughput. Prints a table and
//! asserts the 2-relay case reaches at least 1.6× the single-relay rate.
//!
//! Run with: cargo test -p orbit-relay --test agg_bench -- --ignored --nocapture

use orbit_relay::RelayCore;
use orbit_transport::{ReceiverSession, SenderSession};
use std::sync::Arc;
use tokio::net::TcpListener;

const KBPS: u64 = 2000; // per-relay egress cap (2 MB/s)
const MB: usize = 1024 * 1024;
const PAYLOAD_MIB: usize = 16;
const SYMBOL_SIZE: usize = 4096;

async fn spawn_relay(kbps: Option<u64>) -> (String, Arc<RelayCore>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let core = Arc::new(RelayCore::with_throttle(kbps));
    let relay = core.clone();
    tokio::spawn(async move {
        let _ = relay.run(listener).await;
    });
    (format!("ws://{addr}"), core)
}

struct Row {
    n_relays: usize,
    direct: bool,
    mi_b_s: f64,
    symbols: u64,
    via_direct: u64,
    via_relay: u64,
    relayed: u64,
}

async fn run_case(n_relays: usize, direct: bool, payload: Vec<u8>) -> Row {
    let mut urls = Vec::new();
    let mut cores = Vec::new();
    for _ in 0..n_relays {
        let (url, core) = spawn_relay(Some(KBPS)).await;
        urls.push(url);
        cores.push(core);
    }
    let session_id: u64 = rand::random();
    let listen = direct.then(|| "127.0.0.1:0".to_string());

    let sender = {
        let urls = urls.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut s = SenderSession::connect(
                &urls,
                session_id,
                "bench.bin".to_string(),
                payload,
                SYMBOL_SIZE,
                None,
            )
            .await?;
            let stats = s.run().await?;
            anyhow::Ok(stats)
        })
    };
    let receiver = tokio::spawn(async move {
        let mut r = ReceiverSession::connect(&urls, session_id, listen, None).await?;
        let (data, stats) = r.run().await?;
        anyhow::Ok((data, stats))
    });

    let (sender_res, receiver_res) = tokio::join!(sender, receiver);
    let relayed: u64 = cores.iter().map(|c| c.symbols_relayed()).sum();
    let receiver_outcome = match &receiver_res {
        Ok(Ok((data, stats))) => format!(
            "decoded {} B, symbols={}, via_relay={}, via_direct={}",
            data.len(), stats.symbols, stats.via_relay, stats.via_direct
        ),
        Ok(Err(e)) => format!("failed: {e}"),
        Err(e) => format!("panicked: {e}"),
    };
    let sstats = match sender_res.expect("sender task") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sender failed: {e:?} (receiver: {receiver_outcome})");
            panic!("sender failed");
        }
    };
    let (data, _rstats) = receiver_res.expect("receiver task").expect("receiver failed");
    assert_eq!(data, payload, "received payload must match original");

    let mi_b_s = sstats.bytes as f64 / MB as f64 / sstats.elapsed.as_secs_f64();
    eprintln!(
        "case {n_relays} relay(s){}: {:.2} MiB/s in {:.2}s (symbols={})",
        if direct { " + direct" } else { "" },
        mi_b_s,
        sstats.elapsed.as_secs_f64(),
        sstats.symbols,
    );
    Row {
        n_relays,
        direct,
        mi_b_s,
        symbols: sstats.symbols,
        via_direct: sstats.via_direct,
        via_relay: sstats.via_relay,
        relayed,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "bench: run explicitly with --ignored"]
async fn aggregation_bench() {
    let payload: Vec<u8> = (0..PAYLOAD_MIB * MB).map(|i| (i % 251) as u8).collect();
    let cases = [(1usize, false), (2, false), (3, false), (3, true)];
    let mut rows = Vec::new();
    for (n, direct) in cases {
        println!("=== case {n} relay(s){} ===", if direct { " + direct" } else { "" });
        rows.push(run_case(n, direct, payload.clone()).await);
    }

    println!();
    println!("orbit-transfer · aggregation bench (relays throttled at {KBPS} kbps each)");
    println!("payload: {PAYLOAD_MIB} MiB · symbol size: {SYMBOL_SIZE} B");
    println!(
        "{:>8} {:>6} {:>10} {:>9} {:>10} {:>10} {:>8}",
        "relays", "direct", "MiB/s", "symbols", "via_direct", "via_relay", "relayed"
    );
    for r in &rows {
        println!(
            "{:>8} {:>6} {:>10.2} {:>9} {:>10} {:>10} {:>8}",
            r.n_relays, r.direct, r.mi_b_s, r.symbols, r.via_direct, r.via_relay, r.relayed
        );
    }

    let one = rows[0].mi_b_s;
    let two = rows[1].mi_b_s;
    let factor = two / one;
    println!();
    println!("aggregation factor (2 relays vs 1): {factor:.2}x");
    println!("3 relays vs 1: {:.2}x", rows[2].mi_b_s / one);

    assert!(
        factor >= 1.6,
        "2 relays must aggregate >= 1.6x a single relay (got {factor:.2}x)"
    );
}