//! Throttle calibration: measures the raw relay egress rate with a plain
//! reader (no sessions, no fountain decode) to isolate the limiter.
//!
//! Run with: cargo test -p orbit-relay --test throttle_cal -- --ignored --nocapture

use bytes::Bytes;
use orbit_protocol::wire::{OrbitMessage, ROLE_SENDER};
use orbit_relay::RelayCore;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

async fn spawn_relay(kbps: u64) -> (String, Arc<RelayCore>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let core = Arc::new(RelayCore::with_throttle(Some(kbps)));
    let relay = core.clone();
    tokio::spawn(async move {
        let _ = relay.run(listener).await;
    });
    (format!("ws://{addr}"), core)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "calibration: run explicitly with --ignored"]
async fn throttle_calibration() {
    let kbps = 2000u64;
    let (url, core) = spawn_relay(kbps).await;
    let session_id: u64 = rand::random();
    {
        let (sink, _stream) = orbit_transport::connect(&url).await.unwrap();
        sink.send(&OrbitMessage::Hello {
            session_id,
            role: ROLE_SENDER,
        })
        .await
        .unwrap();
        // Keep the sender registration alive (Hello already registered it).
        tokio::spawn(async move {
            let _ = &sink;
            std::future::pending::<()>().await;
        });
    }
    // A receiver role registers too, so symbols are routed forward; its
    // stream is the one the throttled sink feeds.
    let mut stream = {
        let (sink, stream) = orbit_transport::connect(&url).await.unwrap();
        sink.send(&OrbitMessage::Hello {
            session_id,
            role: orbit_protocol::wire::ROLE_RECEIVER,
        })
        .await
        .unwrap();
        tokio::spawn(async move {
            let _ = &sink;
            std::future::pending::<()>().await;
        });
        stream
    };

    let data = vec![0x42u8; 4096];
    let (sink, _stream2) = orbit_transport::connect(&url).await.unwrap();
    let sender_task = tokio::spawn(async move {
        let mut i = 0u32;
        loop {
            sink.send(&OrbitMessage::Symbol {
                session_id,
                esi: i,
                data: Bytes::from(data.clone()),
            })
            .await
            .unwrap();
            i += 1;
        }
    });

    let start = std::time::Instant::now();
    let mut count = 0u64;
    while start.elapsed() < Duration::from_secs(5) {
        match tokio::time::timeout(Duration::from_millis(500), stream.recv()).await {
            Ok(Ok(Some(_))) => count += 1,
            Ok(Ok(None)) | Ok(Err(_)) => {
                eprintln!("stream ended");
                break;
            }
            Err(_) => break,
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let relayed = core.symbols_relayed();
    eprintln!(
        "throttle {kbps} kbps: received {count} symbols in {elapsed:.2}s = {:.0} symbols/s = {:.2} MiB/s (relay counted {relayed})",
        count as f64 / elapsed,
        count as f64 * 4096.0 / 1048576.0 / elapsed
    );
    sender_task.abort();
    assert!(count as f64 / elapsed > 300.0, "expected >= 300 symbols/s at 2000 kbps");
}

/// Measures the per-connection throttle rate with N relays running in
/// parallel (no sessions/decode), to isolate throttle contention.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "calibration: run explicitly with --ignored"]
async fn throttle_concurrency_calibration() {
    let kbps = 2000u64;
    let n = 3usize;
    let mut tasks = Vec::new();
    for _ in 0..n {
        let (url, _core) = spawn_relay(kbps).await;
        let session_id: u64 = rand::random();
        tasks.push(tokio::spawn(async move {
            {
                let (sink, _stream) = orbit_transport::connect(&url).await.unwrap();
                sink.send(&OrbitMessage::Hello {
                    session_id,
                    role: ROLE_SENDER,
                })
                .await
                .unwrap();
                tokio::spawn(async move {
                    let _ = &sink;
                    std::future::pending::<()>().await;
                });
            }
            let mut stream = {
                let (sink, stream) = orbit_transport::connect(&url).await.unwrap();
                sink.send(&OrbitMessage::Hello {
                    session_id,
                    role: orbit_protocol::wire::ROLE_RECEIVER,
                })
                .await
                .unwrap();
                tokio::spawn(async move {
                    let _ = &sink;
                    std::future::pending::<()>().await;
                });
                stream
            };
            let data = vec![0x42u8; 4096];
            let (sink, _s2) = orbit_transport::connect(&url).await.unwrap();
            let sender_task = tokio::spawn(async move {
                let mut i = 0u32;
                loop {
                    sink.send(&OrbitMessage::Symbol {
                        session_id,
                        esi: i,
                        data: Bytes::from(data.clone()),
                    })
                    .await
                    .unwrap();
                    i += 1;
                }
            });
            let start = std::time::Instant::now();
            let mut count = 0u64;
            while start.elapsed() < Duration::from_secs(5) {
                match tokio::time::timeout(Duration::from_millis(500), stream.recv()).await {
                    Ok(Ok(Some(_))) => count += 1,
                    _ => break,
                }
            }
            sender_task.abort();
            (count, start.elapsed().as_secs_f64())
        }));
    }
    let mut per_relay = Vec::new();
    let mut total = 0.0;
    for t in tasks {
        let (count, elapsed) = t.await.unwrap();
        let rate = count as f64 / elapsed;
        per_relay.push(rate);
        total += rate;
    }
    eprintln!("throttle concurrency n={n}: per-relay rates = {per_relay:?} -> total {total:.0}/s");
    eprintln!("  (expected ~{:.0}/s total if independent)", 500.0 * n as f64);
}