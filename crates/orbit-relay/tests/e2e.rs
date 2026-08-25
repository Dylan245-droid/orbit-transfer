use orbit_relay::RelayCore;
use orbit_transport::{ReceiverSession, SenderSession};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

/// Mirror of `orbit_fountain::precode::s_for` (relay does not depend on the fountain crate).
fn s_for(k: usize) -> usize {
    if k <= 2 {
        return 2;
    }
    (k / 100 + 10).max(2)
}

/// Spawns a relay throttled to `kbps` egress, so the direct P2P path (full
/// loopback speed) deterministically outpaces the relay. Returns the WS URL.
async fn spawn_throttled_relay(kbps: u64) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let core = Arc::new(RelayCore::with_throttle(Some(kbps)));
    tokio::spawn(async move { let _ = core.run(listener).await; });
    format!("ws://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_transfer_through_relay() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let core = Arc::new(RelayCore::new());
    let relay_task = tokio::spawn(async move { core.run(listener).await });

    let url = format!("ws://{addr}");
    let urls = vec![url.clone()];
    let session_id: u64 = rand::random();
    let symbol_size = 64 * 1024;
    let payload: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();

    let sender = {
        let urls = urls.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut s = SenderSession::connect(
                &urls,
                session_id,
                "test.bin".to_string(),
                payload,
                symbol_size,
                None,
            )
            .await?;
            let stats = s.run().await?;
            anyhow::Ok(stats)
        })
    };

    let receiver = tokio::spawn(async move {
        let mut r = ReceiverSession::connect(&urls, session_id, None, None).await?;
        let (data, stats) = r.run().await?;
        anyhow::Ok((data, stats))
    });

    let (sender_res, receiver_res) = tokio::join!(sender, receiver);
    let sstats = sender_res??;
    let (data, rstats) = receiver_res??;

    assert_eq!(data, payload, "received payload must match original");
    let k = payload.len() / symbol_size + usize::from(payload.len() % symbol_size != 0);
    let l = k + s_for(k);
    assert!(
        sstats.symbols >= k as u64,
        "sender must emit at least K symbols"
    );
    assert!(
        rstats.symbols <= sstats.symbols,
        "receiver cannot receive more symbols than sent"
    );
    assert!(
        rstats.symbols <= l as u64,
        "lossless delivery must decode within the systematic phase (L = K + S symbols)"
    );

    relay_task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_receiver_joins_after_sender() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let core = Arc::new(RelayCore::new());
    let relay_task = {
        let core = core.clone();
        tokio::spawn(async move { core.run(listener).await })
    };

    let url = format!("ws://{addr}");
    let urls = vec![url.clone()];
    let session_id: u64 = rand::random();
    let payload = vec![0x5Au8; 512 * 1024];
    let symbol_size = 8192;
    // Lower bound on symbols the sender will emit before receiving READY.
    let expected_symbols = payload.len() / symbol_size + 10;

    // Sender streams all symbols before the receiver connects:
    // the relay must buffer them and flush on registration.
    let sender = {
        let urls = urls.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut s = SenderSession::connect(
                &urls,
                session_id,
                "late.bin".to_string(),
                payload,
                symbol_size,
                None,
            )
            .await?;
            let stats = s.run().await?;
            anyhow::Ok(stats)
        })
    };

    // Wait until the relay has buffered every symbol the sender will produce.
    let deadline = Instant::now() + Duration::from_secs(5);
    while core.symbols_relayed() < expected_symbols as u64 {
        if Instant::now() > deadline {
            anyhow::bail!("relay did not buffer all symbols in time");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut receiver = ReceiverSession::connect(&urls, session_id, None, None).await?;
    let (data, rstats) = receiver.run().await?;
    let sstats = sender.await??;

    assert_eq!(data, payload, "received payload must match original");
    assert!(
        rstats.symbols <= sstats.symbols,
        "receiver cannot receive more symbols than sent"
    );

    relay_task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_multipath_direct_and_relay() -> anyhow::Result<()> {
    let url = spawn_throttled_relay(4096).await;
    let urls = vec![url.clone()];
    let session_id: u64 = rand::random();
    let symbol_size = 16 * 1024;
    let payload: Vec<u8> = (0..16 * 1024 * 1024).map(|i| (i % 97) as u8).collect();

    let sender = {
        let urls = urls.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut s = SenderSession::connect(
                &urls,
                session_id,
                "mp.bin".to_string(),
                payload,
                symbol_size,
                None,
            )
            .await?;
            let stats = s.run().await?;
            anyhow::Ok(stats)
        })
    };

    let receiver = tokio::spawn(async move {
        let mut r = ReceiverSession::connect(&urls, session_id, Some("127.0.0.1:0".to_string()), None)
            .await?;
        let (data, stats) = r.run().await?;
        anyhow::Ok((data, stats))
    });

    let (sender_res, receiver_res) = tokio::join!(sender, receiver);
    let sstats = sender_res??;
    let (data, rstats) = receiver_res??;

    assert_eq!(data, payload, "received payload must match original");
    assert!(
        sstats.via_direct > 0,
        "P2P path must carry at least one symbol"
    );
    assert!(
        sstats.via_relay > 0,
        "relay path must carry at least one symbol"
    );
    assert_eq!(
        sstats.symbols,
        sstats.via_direct + sstats.via_relay,
        "every emitted symbol must have used exactly one path"
    );
    assert!(
        rstats.via_direct > 0,
        "receiver must have decoded symbols from the direct path"
    );
    assert!(
        rstats.via_direct <= sstats.via_direct,
        "receiver cannot count more direct symbols than the sender sent"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_multipath_quic_direct_and_relay() -> anyhow::Result<()> {
    let url = spawn_throttled_relay(4096).await;
    let urls = vec![url.clone()];
    let session_id: u64 = rand::random();
    let symbol_size = 16 * 1024;
    let payload: Vec<u8> = (0..16 * 1024 * 1024).map(|i| (i % 97) as u8).collect();

    let sender = {
        let urls = urls.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut s = SenderSession::connect(
                &urls,
                session_id,
                "mpq.bin".to_string(),
                payload,
                symbol_size,
                None,
            )
            .await?;
            let stats = s.run().await?;
            anyhow::Ok(stats)
        })
    };

    let receiver = tokio::spawn(async move {
        let mut r = ReceiverSession::connect(
            &urls,
            session_id,
            Some("quic://127.0.0.1:0".to_string()),
            None,
        )
        .await?;
        let (data, stats) = r.run().await?;
        anyhow::Ok((data, stats))
    });

    let (sender_res, receiver_res) = tokio::join!(sender, receiver);
    let sstats = sender_res??;
    let (data, rstats) = receiver_res??;

    assert_eq!(data, payload, "received payload must match original");
    assert!(
        sstats.via_direct > 0,
        "QUIC direct path must carry at least one symbol"
    );
    assert!(
        sstats.via_relay > 0,
        "relay path must carry at least one symbol"
    );
    assert!(
        rstats.via_direct > 0,
        "receiver must have decoded symbols from the QUIC direct path"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_encrypted_transfer() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let core = Arc::new(RelayCore::new());
    let relay_task = tokio::spawn(async move { core.run(listener).await });

    let url = format!("ws://{addr}");
    let urls = vec![url.clone()];
    let session_id: u64 = rand::random();
    let symbol_size = 8 * 1024;
    let payload: Vec<u8> = (0..1 * 1024 * 1024).map(|i| (i % 251) as u8).collect();

    let sender = {
        let urls = urls.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut s = SenderSession::connect(
                &urls,
                session_id,
                "secret.bin".to_string(),
                payload,
                symbol_size,
                Some("correct horse battery staple".to_string()),
            )
            .await?;
            let stats = s.run().await?;
            anyhow::Ok(stats)
        })
    };

    let receiver = tokio::spawn(async move {
        let mut r = ReceiverSession::connect(
            &urls,
            session_id,
            None,
            Some("correct horse battery staple".to_string()),
        )
        .await?;
        let (data, stats) = r.run().await?;
        anyhow::Ok((data, stats))
    });

    let (sender_res, receiver_res) = tokio::join!(sender, receiver);
    sender_res??;
    let (data, _rstats) = receiver_res??;

    assert_eq!(data, payload, "decrypted payload must match original");

    relay_task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_multi_relay_aggregation() -> anyhow::Result<()> {
    let listener_a = TcpListener::bind("127.0.0.1:0").await?;
    let addr_a = listener_a.local_addr()?;
    let core_a = Arc::new(RelayCore::new());
    let relay_a = {
        let core = core_a.clone();
        tokio::spawn(async move { core.run(listener_a).await })
    };
    let listener_b = TcpListener::bind("127.0.0.1:0").await?;
    let addr_b = listener_b.local_addr()?;
    let core_b = Arc::new(RelayCore::new());
    let relay_b = {
        let core = core_b.clone();
        tokio::spawn(async move { core.run(listener_b).await })
    };

    let urls = vec![format!("ws://{addr_a}"), format!("ws://{addr_b}")];
    let session_id: u64 = rand::random();
    let symbol_size = 16 * 1024;
    let payload: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 89) as u8).collect();

    let sender = {
        let urls = urls.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut s = SenderSession::connect(
                &urls,
                session_id,
                "multi.bin".to_string(),
                payload,
                symbol_size,
                None,
            )
            .await?;
            let stats = s.run().await?;
            anyhow::Ok(stats)
        })
    };

    let receiver = tokio::spawn(async move {
        let mut r = ReceiverSession::connect(&urls, session_id, None, None).await?;
        let (data, stats) = r.run().await?;
        anyhow::Ok((data, stats))
    });

    let (sender_res, receiver_res) = tokio::join!(sender, receiver);
    let sstats = sender_res??;
    let (data, rstats) = receiver_res??;

    assert_eq!(data, payload, "received payload must match original");
    assert!(
        sstats.via_relay >= 2,
        "bandwidth must be aggregated over both relays (got {})",
        sstats.via_relay
    );
    assert!(
        core_a.symbols_relayed() > 0 && core_b.symbols_relayed() > 0,
        "both relays must carry at least one symbol (a={}, b={})",
        core_a.symbols_relayed(),
        core_b.symbols_relayed()
    );
    assert!(
        rstats.via_relay >= 2,
        "receiver must decode symbols coming from both relays (got {})",
        rstats.via_relay
    );

    relay_a.abort();
    relay_b.abort();
    Ok(())
}
