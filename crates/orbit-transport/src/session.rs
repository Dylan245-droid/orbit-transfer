use crate::client::{connect, RelaySink};
use crate::direct::{DirectWriter, P2pChannel, P2pListener};
use crate::scheduler::MultiPathScheduler;
use orbit_crypto::SessionCipher;
use orbit_fountain::{EncodedSymbol, FountainDecoder, FountainEncoder};
use orbit_protocol::wire::{OrbitMessage, ROLE_RECEIVER, ROLE_SENDER};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Per-path queue depth (symbols). Deep enough that each path can absorb a
/// burst, small enough that a dead path is noticed quickly.
const PATH_QUEUE_DEPTH: usize = 1024;

#[derive(Debug, Clone)]
pub struct TransferStats {
    pub bytes: u64,
    pub symbols: u64,
    pub elapsed: Duration,
    pub overhead_ratio: f64,
    pub via_direct: u64,
    pub via_relay: u64,
}

impl TransferStats {
    pub fn mbps(&self) -> f64 {
        self.bytes as f64 * 8.0 / 1e6 / self.elapsed.as_secs_f64()
    }

    pub fn symbols_per_sec(&self) -> f64 {
        self.symbols as f64 / self.elapsed.as_secs_f64()
    }
}

/// Event observed by one of the sender's background watchers.
enum SenderEvent {
    /// Receiver decoded the full payload; stop emitting.
    Ready,
    /// Receiver advertised a reachable P2P address; use it when possible.
    Direct(P2pChannel),
}

/// Sender side of a transfer session.
///
/// The sender is *truly rateless*: it keeps emitting new encoded symbols
/// (fountain symbols with unbounded ESI) until the receiver signals READY.
/// No fixed overhead is assumed, so any loss rate is tolerated as long as
/// the link eventually delivers K + epsilon symbols.
///
/// Symbols are spread over up to N edge relays and one optional P2P direct
/// channel by the `MultiPathScheduler`; a failed relay is disabled on the
/// fly and its load is re-routed over the surviving paths.
///
/// Abort when the emission loop is blocked (every path queue full) with no
/// successful send for this long: the receiver's READY always arrives well
/// within this window while the pipes are draining, so a timeout means the
/// receiver (or every path) is gone.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A background watcher is spawned per relay connection: it watches the
/// incoming stream for READY (ack) and DIRECT (P2P address) events and
/// forwards them through a channel, so the fast emission loop can never
/// miss them (even when every send completes instantly).
///
/// Symbols are handed to per-path writer tasks through bounded queues, so
/// the emission loop never blocks on a slow link: each path drains at its
/// own rate and the aggregate throughput is the sum of all live paths.
pub struct SenderSession {
    path_tx: Vec<mpsc::Sender<OrbitMessage>>,
    path_alive: Arc<Vec<AtomicBool>>,
    events: tokio::sync::mpsc::Receiver<SenderEvent>,
    encoder: FountainEncoder,
    cipher: Option<SessionCipher>,
    session_id: u64,
    filename: String,
}

impl SenderSession {
    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        urls: &[String],
        session_id: u64,
        filename: String,
        payload: Vec<u8>,
        symbol_size: usize,
        secret: Option<String>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!urls.is_empty(), "at least one relay URL is required");

        let (notify_tx, events) = tokio::sync::mpsc::channel(urls.len() + 2);
        let direct_up = Arc::new(AtomicBool::new(false));
        let mut sinks = Vec::with_capacity(urls.len());

        for url in urls {
            let (sink, mut stream) = connect(url).await?;
            sink.send(&OrbitMessage::Hello {
                session_id,
                role: ROLE_SENDER,
            })
            .await?;
            let notify_tx = notify_tx.clone();
            let direct_up = direct_up.clone();
            tokio::spawn(async move {
                // Only reconnect on advertisement until one link is
                // established: the receiver re-advertises periodically, and
                // reconnecting over a healthy link would strand symbols on
                // an unaccepted connection.
                loop {
                    match stream.recv().await {
                        Ok(Some(OrbitMessage::Ready { .. })) => {
                            notify_tx.send(SenderEvent::Ready).await.ok();
                            break;
                        }
                        Ok(Some(OrbitMessage::Direct { addr, .. })) => {
                            if !direct_up.swap(true, Ordering::SeqCst) {
                                let connected = tokio::time::timeout(
                                    Duration::from_secs(2),
                                    P2pChannel::connect(&addr),
                                )
                                .await;
                                match connected {
                                    Ok(Ok(ch)) => {
                                        tracing::info!("P2P direct link established to {addr}");
                                        if notify_tx.send(SenderEvent::Direct(ch)).await.is_err() {
                                            break;
                                        }
                                    }
                                    _ => {
                                        tracing::warn!("P2P direct link to {addr} failed; relay-only");
                                        direct_up.store(false, Ordering::SeqCst);
                                    }
                                }
                            }
                        }
                        Ok(Some(_)) => continue,
                        Ok(None) | Err(_) => break,
                    }
                }
            });
            sinks.push(Some(sink));
        }

        let encoder = FountainEncoder::new(&payload, symbol_size);
        sinks[0]
            .as_ref()
            .expect("first relay connection is alive")
            .send(&OrbitMessage::Meta {
                session_id,
                filename: filename.clone(),
                size: payload.len() as u64,
                symbol_size: encoder.symbol_size() as u32,
                k: encoder.k() as u32,
                checksum: encoder.checksum(),
            })
            .await?;

        // One bounded queue + writer task per relay. The emission loop only
        // queues (never awaits the network), so each path drains at its own
        // rate and the aggregate is the sum of all live paths.
        let path_alive = Arc::new((0..sinks.len()).map(|_| AtomicBool::new(true)).collect::<Vec<_>>());
        let mut path_tx = Vec::with_capacity(sinks.len());
        for (i, sink) in sinks.into_iter().enumerate() {
            let sink = sink.expect("relay connection is alive");
            let (tx, mut rx) = mpsc::channel::<OrbitMessage>(PATH_QUEUE_DEPTH);
            let alive = Arc::clone(&path_alive);
            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    if sink.send(&msg).await.is_err() {
                        tracing::warn!("relay {i} send failed; disabling it");
                        alive[i].store(false, Ordering::SeqCst);
                        return;
                    }
                }
            });
            path_tx.push(tx);
        }

        Ok(Self {
            path_tx,
            path_alive,
            events,
            encoder,
            cipher: secret.map(|s| SessionCipher::new(&s)),
            session_id,
            filename,
        })
    }

    /// Queues a symbol on the best available path, starting with `first`.
    /// Returns `false` when every path queue is full or dead.
    fn dispatch(&self, msg: &OrbitMessage, first: usize) -> bool {
        let n = self.path_tx.len();
        for offset in 0..n {
            let j = (first + offset) % n;
            if !self.path_alive[j].load(Ordering::SeqCst) {
                continue;
            }
            if self.path_tx[j].try_send(msg.clone()).is_ok() {
                return true;
            }
        }
        false
    }

    fn any_path_alive(&self) -> bool {
        self.path_alive.iter().any(|a| a.load(Ordering::SeqCst))
    }

    /// Dispatches a symbol, or blocks the loop when every path is saturated
    /// (yields, retries the same symbol). Aborts after [`STALL_TIMEOUT`]
    /// without any successful send, which means the receiver's READY could
    /// not arrive because the pipes are not draining.
    async fn dispatch_or_wait(
        &mut self,
        msg: &OrbitMessage,
        first: usize,
        last_send: &mut Instant,
    ) -> anyhow::Result<bool> {
        if self.dispatch(msg, first) {
            *last_send = Instant::now();
            return Ok(true);
        }
        if !self.any_path_alive() {
            anyhow::bail!("all relays unreachable");
        }
        if last_send.elapsed() > STALL_TIMEOUT {
            anyhow::bail!("receiver did not acknowledge; no send progress for {STALL_TIMEOUT:?}");
        }
        tokio::task::yield_now().await;
        Ok(false)
    }

    /// Streams symbols until the receiver decodes and acknowledges with READY.
    ///
    /// Symbols are distributed across the direct P2P channel and the N
    /// relays by the scheduler; a failed P2P send falls back to a relay.
    pub async fn run(&mut self) -> anyhow::Result<TransferStats> {
        let start = Instant::now();
        let k = self.encoder.k() as u32;
        let mut scheduler = MultiPathScheduler::new(1, 1).with_relays(self.path_tx.len());
        let mut direct: Option<DirectWriter> = None;
        let mut last_send = Instant::now();
        let mut esi = 0u32;
        let mut sent = 0u64;
        let mut via_direct = 0u64;
        let mut via_relay = 0u64;
        let mut finished = false;

        while !finished {
            // Events first: a READY that already arrived must always beat
            // the symbol cap (the last send may overlap the READY arrival).
            while let Ok(evt) = self.events.try_recv() {
                match evt {
                    SenderEvent::Ready => finished = true,
                    SenderEvent::Direct(ch) => {
                        let (writer, _reader) = ch.into_pipeline();
                        direct = Some(writer);
                    }
                }
            }
            if finished {
                break;
            }
            if !direct.is_some() && !self.any_path_alive() {
                anyhow::bail!("all relays unreachable");
            }

            let sym = self.encoder.encode_symbol(esi);
            let data: bytes::Bytes = match &self.cipher {
                Some(c) => c.seal_symbol(self.session_id, esi, &sym.data).into(),
                None => sym.data.into(),
            };
            let msg = OrbitMessage::Symbol {
                session_id: self.session_id,
                esi,
                data,
            };

            match scheduler.pick(esi) {
                Some(i) => {
                    if !self.dispatch_or_wait(&msg, i, &mut last_send).await? {
                        continue;
                    }
                    via_relay += 1;
                }
                None => match direct.as_ref() {
                    Some(w) => {
                        if w.try_send(&msg) {
                            last_send = Instant::now();
                            via_direct += 1;
                        } else if w.is_alive() {
                            // Queue momentarily full: keep the path alive and
                            // fall back to a relay for this symbol only.
                            if !self.dispatch_or_wait(&msg, 0, &mut last_send).await? {
                                continue;
                            }
                            via_relay += 1;
                        } else {
                            tracing::warn!("P2P link dead at esi {esi}; disabling it");
                            direct = None;
                            if !self.dispatch_or_wait(&msg, 0, &mut last_send).await? {
                                continue;
                            }
                            via_relay += 1;
                        }
                    }
                    None => {
                        if !self.dispatch_or_wait(&msg, 0, &mut last_send).await? {
                            continue;
                        }
                        via_relay += 1;
                    }
                },
            }
            sent += 1;
            esi += 1;
        }

        Ok(TransferStats {
            bytes: self.encoder.payload_len() as u64,
            symbols: sent,
            elapsed: start.elapsed(),
            overhead_ratio: sent as f64 / k as f64,
            via_direct,
            via_relay,
        })
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }
}

/// Receiver side of a transfer session: decodes fountain symbols on the fly
/// until the payload is fully reconstructed and checksum-verified, then
/// signals READY so the sender can stop emitting.
///
/// The receiver connects to up to N relays and merges all their streams
/// (plus the optional P2P direct channel) into one decode loop; duplicated
/// symbols are harmless for a rateless decoder, and the first Meta to
/// arrive (from any relay) initializes the decoder.
///
/// If a `listen_addr` is provided, the receiver binds a TCP listener,
/// advertises it to the sender via every relay (DIRECT), and merges
/// incoming P2P symbols into the decode loop. If P2P never materializes,
/// the session degrades gracefully to relay-only.
pub struct ReceiverSession {
    sinks: Vec<RelaySink>,
    relay_rx: tokio::sync::mpsc::Receiver<(usize, anyhow::Result<Option<OrbitMessage>>)>,
    n_relays: usize,
    relays_closed: usize,
    direct_rx: Option<tokio::sync::mpsc::Receiver<OrbitMessage>>,
    cipher: Option<SessionCipher>,
    decoder: Option<FountainDecoder>,
    pending: VecDeque<(u32, bytes::Bytes)>,
    session_id: u64,
    filename: Option<String>,
}

enum RecvPath {
    Relay(Option<(usize, anyhow::Result<Option<OrbitMessage>>)>),
    Direct(Option<OrbitMessage>),
}

impl ReceiverSession {
    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        urls: &[String],
        session_id: u64,
        listen_addr: Option<String>,
        secret: Option<String>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!urls.is_empty(), "at least one relay URL is required");

        let mut sinks = Vec::with_capacity(urls.len());
        let mut streams = Vec::with_capacity(urls.len());
        for url in urls {
            let (sink, stream) = connect(url).await?;
            sink.send(&OrbitMessage::Hello {
                session_id,
                role: ROLE_RECEIVER,
            })
            .await?;
            sinks.push(sink);
            streams.push(stream);
        }
        let n_relays = sinks.len();

        let direct_rx = if let Some(addr) = listen_addr {
            match P2pListener::bind(&addr).await {
                Ok((local, listener)) => {
                    let (tx, rx) = tokio::sync::mpsc::channel(1024);
                    let adv_sinks = sinks.clone();
                    let sid = session_id;
                    let prefix = if addr.starts_with("quic://") { "quic://" } else { "" };
                    let advertised = format!("{prefix}{local}");
                    // Re-advertise the P2P address on every relay until the
                    // sender connects, covering the race where the sender
                    // registers at the relay after us.
                    tokio::spawn(async move {
                        loop {
                            for s in &adv_sinks {
                                if s.send(&OrbitMessage::Direct {
                                    session_id: sid,
                                    addr: advertised.clone(),
                                })
                                .await
                                .is_err()
                                {
                                    continue;
                                }
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    });
                    // Accept connections in a dedicated loop. For QUIC the
                    // `accept()` future must never be dropped mid-handshake:
                    // dropping an `Incoming` rejects the connection. Each
                    // accepted stream is forwarded independently, and a
                    // reconnecting sender always finds a live listener.
                    tokio::spawn(async move {
                        let mut listener = listener;
                        loop {
                            match listener.accept().await {
                                Ok(direct) => {
                                    let tx = tx.clone();
                                    tokio::spawn(async move {
                                        let mut direct = direct;
                                        loop {
                                            match direct.recv().await {
                                                Ok(Some(msg)) => {
                                                    if tx.send(msg).await.is_err() {
                                                        return;
                                                    }
                                                }
                                                _ => return,
                                            }
                                        }
                                    });
                                }
                                Err(_) => return,
                            }
                        }
                    });
                    tracing::info!(
                        "advertising P2P address {local} via {n_relays} relay(s)"
                    );
                    Some(rx)
                }
                Err(e) => {
                    tracing::warn!("could not bind P2P listener on {addr}: {e}; relay-only");
                    None
                }
            }
        } else {
            None
        };

        // One reader task per relay connection: frames are re-emitted on a
        // single channel so the decode loop can select over all relays.
        let (relay_tx, relay_rx) =
            tokio::sync::mpsc::channel::<(usize, anyhow::Result<Option<OrbitMessage>>)>(1024);
        for (i, mut stream) in streams.into_iter().enumerate() {
            let tx = relay_tx.clone();
            tokio::spawn(async move {
                loop {
                    match stream.recv().await {
                        Ok(Some(msg)) => {
                            if tx.send((i, Ok(Some(msg)))).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => {
                            let _ = tx.send((i, Ok(None))).await;
                            return;
                        }
                        Err(e) => {
                            let _ = tx.send((i, Err(e))).await;
                            return;
                        }
                    }
                }
            });
        }
        drop(relay_tx);

        Ok(Self {
            sinks,
            relay_rx,
            n_relays,
            relays_closed: 0,
            direct_rx,
            cipher: secret.map(|s| SessionCipher::new(&s)),
            decoder: None,
            pending: VecDeque::new(),
            session_id,
            filename: None,
        })
    }

    fn handle_symbol(&mut self, esi: u32, data: bytes::Bytes) -> anyhow::Result<bool> {
        let Some(decoder) = self.decoder.as_mut() else {
            // A P2P symbol (or a symbol from another relay) can beat the
            // relayed Meta over the network; keep it buffered until the
            // Meta arrives (order never matters for a rateless decoder,
            // and duplicates are harmless).
            self.pending.push_back((esi, data));
            return Ok(false);
        };
        let open = match &self.cipher {
            Some(c) => c
                .open_symbol(self.session_id, esi, &data)
                .map_err(|e| anyhow::anyhow!("failed to open symbol {esi}: {e}"))?,
            None => data.to_vec(),
        };
        let sym = EncodedSymbol {
            esi,
            symbol_size: decoder.symbol_size(),
            data: open,
        };
        Ok(decoder.add_symbol(sym))
    }

    /// Consumes symbols from all relays and/or the direct P2P channel until
    /// decoding completes, verifies integrity, signals READY.
    pub async fn run(&mut self) -> anyhow::Result<(Vec<u8>, TransferStats)> {
        let start = Instant::now();
        let mut symbols_received = 0u64;
        let mut bytes_received = 0u64;
        let mut via_direct = 0u64;
        let mut via_relay = 0u64;

        loop {
            let recv = {
                let relay_fut = self.relay_rx.recv();
                let direct_fut = async {
                    match self.direct_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending::<Option<OrbitMessage>>().await,
                    }
                };
                tokio::select! {
                    r = relay_fut => RecvPath::Relay(r),
                    d = direct_fut => RecvPath::Direct(d),
                }
            };

            match recv {
                RecvPath::Relay(Some((
                    _,
                    Ok(Some(OrbitMessage::Meta {
                        filename,
                        size,
                        symbol_size,
                        k,
                        checksum,
                        ..
                    })),
                ))) if self.decoder.is_none() => {
                    self.filename = Some(filename);
                    self.decoder = Some(FountainDecoder::new(
                        size as usize,
                        symbol_size as usize,
                        k as usize,
                        checksum,
                    ));
                    let mut completed = false;
                    while let Some((esi, data)) = self.pending.pop_front() {
                        if self.handle_symbol(esi, data)? {
                            completed = true;
                            break;
                        }
                    }
                    if completed {
                        break;
                    }
                }
                RecvPath::Relay(Some((_, Ok(Some(OrbitMessage::Meta { .. }))))) => continue,
                RecvPath::Relay(Some((_, Ok(Some(OrbitMessage::Symbol { esi, data, .. }))))) => {
                    via_relay += 1;
                    symbols_received += 1;
                    bytes_received += data.len() as u64;
                    if self.handle_symbol(esi, data)? {
                        break;
                    }
                }
                RecvPath::Direct(Some(OrbitMessage::Symbol { esi, data, .. })) => {
                    via_direct += 1;
                    symbols_received += 1;
                    bytes_received += data.len() as u64;
                    if self.handle_symbol(esi, data)? {
                        break;
                    }
                }
                RecvPath::Direct(None) => {
                    tracing::warn!("P2P direct link closed; degrading to relay-only");
                    self.direct_rx = None;
                }
                RecvPath::Direct(Some(_)) => continue,
                RecvPath::Relay(Some((_, Ok(Some(OrbitMessage::Done { .. }))))) => {
                    anyhow::bail!("sender aborted the transfer");
                }
                RecvPath::Relay(Some((_, Ok(Some(_))))) => continue,
                RecvPath::Relay(Some((_, Ok(None)))) | RecvPath::Relay(Some((_, Err(_)))) => {
                    self.relays_closed += 1;
                    tracing::warn!(
                        "relay connection closed ({} of {} relays down)",
                        self.relays_closed,
                        self.n_relays
                    );
                    if self.decoder.is_none() && self.relays_closed == self.n_relays {
                        anyhow::bail!("all relay connections closed before Meta arrived");
                    }
                }
                RecvPath::Relay(None) => {
                    if self.decoder.is_none() {
                        anyhow::bail!("all relay connections closed before transfer completed");
                    }
                    if self.direct_rx.is_none() {
                        anyhow::bail!("all relay connections closed before transfer completed");
                    }
                }
            }
        }

        let decoder = self.decoder.as_ref().expect("Meta received");
        let payload = decoder.reconstruct()?;
        let ready = OrbitMessage::Ready {
            session_id: self.session_id,
        };
        for sink in &self.sinks {
            sink.send(&ready).await?;
        }
        // Ask every relay to close its side, then drain all streams until
        // their sockets are fully closed. Closing our socket with unread
        // data (the relays keep flooding symbols until the READY is routed)
        // would emit an RST that purges the in-flight READY from the relays'
        // receive buffers.
        for sink in &self.sinks {
            sink.send_close().await?;
        }
        let remaining = self.n_relays - self.relays_closed;
        match tokio::time::timeout(Duration::from_secs(10), async {
            let mut remaining = remaining;
            while remaining > 0 {
                match self.relay_rx.recv().await {
                    Some((_, Ok(Some(_)))) => {}
                    Some((_, Ok(None))) | Some((_, Err(_))) => remaining -= 1,
                    None => break,
                }
            }
        })
        .await
        {
            Ok(()) => {}
            Err(_) => tracing::warn!("relays did not close within 10s after READY"),
        }

        let elapsed = start.elapsed();
        Ok((
            payload,
            TransferStats {
                bytes: bytes_received,
                symbols: symbols_received,
                elapsed,
                overhead_ratio: symbols_received as f64 / decoder.k() as f64,
                via_direct,
                via_relay,
            },
        ))
    }

    pub fn filename(&self) -> Option<&str> {
        self.filename.as_deref()
    }
}