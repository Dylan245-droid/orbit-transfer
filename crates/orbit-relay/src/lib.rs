use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use orbit_protocol::wire::{OrbitMessage, ROLE_RECEIVER, ROLE_SENDER};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

const MAX_BUFFERED_BYTES: usize = 256 * 1024 * 1024;

/// Raises the Windows system timer resolution to 1 ms so `tokio::time::sleep`
/// is accurate below the default ~15.6 ms quantum (needed by the relay
/// throttle, which paces every symbol in the millisecond range).
#[cfg(target_os = "windows")]
fn init_hires_timer() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        unsafe {
            windows_sys::Win32::Media::timeBeginPeriod(1);
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn init_hires_timer() {}

/// Token-bucket egress limiter simulating a constrained uplink.
struct RateLimiter {
    kbps: u64,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    fn new(kbps: u64) -> Self {
        init_hires_timer();
        Self {
            kbps,
            tokens: 0.0,
            last: Instant::now(),
        }
    }

    async fn throttle_bytes(&mut self, n: usize) {
        let rate = self.kbps as f64 * 1024.0;
        // Cap the accumulated credit: idle time may buy a small burst, but a
        // long idle (e.g. session setup) must not unlock an unbounded run.
        let cap = rate * 0.01; // ~10 ms of egress
        let now = Instant::now();
        // Credit the FULL real elapsed time since the last accounting point.
        // `last` is refreshed here and never after the sleep, so the sleep
        // duration (whatever the actual timer precision) is credited to the
        // next symbol: the long-term rate converges to `rate` regardless of
        // how imprecise tokio's timer is.
        self.tokens = (self.tokens + (now - self.last).as_secs_f64() * rate).min(cap);
        self.last = now;
        if self.tokens >= n as f64 {
            self.tokens -= n as f64;
            return;
        }
        let wait = (n as f64 - self.tokens) / rate;
        self.tokens = 0.0;
        if wait > 0.0 {
            // 1 ms timer resolution (see init_hires_timer): sleeping parks the
            // task, so concurrent throttled relays do not contend for CPU.
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait)).await;
        }
    }
}

struct Room {
    sender: Option<(String, mpsc::Sender<WsMessage>)>,
    receiver: Option<(String, mpsc::Sender<WsMessage>)>,
    buffer: VecDeque<WsMessage>,
    buffered_bytes: usize,
    pending_direct: Option<WsMessage>,
    receiver_done: bool,
}

impl Room {
    fn new() -> Self {
        Self {
            sender: None,
            receiver: None,
            buffer: VecDeque::new(),
            buffered_bytes: 0,
            pending_direct: None,
            receiver_done: false,
        }
    }

    fn push_buffer(&mut self, msg: WsMessage) {
        let size = msg.len();
        while self.buffered_bytes + size > MAX_BUFFERED_BYTES {
            if let Some(old) = self.buffer.pop_front() {
                self.buffered_bytes = self.buffered_bytes.saturating_sub(old.len());
            } else {
                break;
            }
        }
        self.buffered_bytes += size;
        self.buffer.push_back(msg);
    }

    /// Moves all buffered messages out, leaving the buffer empty.
    fn drain_buffer(&mut self) -> Vec<WsMessage> {
        let mut out = Vec::with_capacity(self.buffer.len());
        while let Some(msg) = self.buffer.pop_front() {
            self.buffered_bytes = self.buffered_bytes.saturating_sub(msg.len());
            out.push(msg);
        }
        out
    }
}

/// Edge relay: routes encoded symbols between a sender and a receiver
/// identified by a shared session id.
pub struct RelayCore {
    rooms: Arc<Mutex<HashMap<u64, Room>>>,
    bytes_relayed: AtomicU64,
    symbols_relayed: AtomicU64,
    throttle_kbps: Option<u64>,
}

impl RelayCore {
    pub fn new() -> Self {
        Self::with_throttle(None)
    }

    /// `throttle_kbps` caps the egress rate of every connection
    /// (per-connection token bucket), simulating a constrained uplink.
    pub fn with_throttle(throttle_kbps: Option<u64>) -> Self {
        Self {
            rooms: Arc::new(Mutex::new(HashMap::new())),
            bytes_relayed: AtomicU64::new(0),
            symbols_relayed: AtomicU64::new(0),
            throttle_kbps,
        }
    }

    pub fn bytes_relayed(&self) -> u64 {
        self.bytes_relayed.load(Ordering::Relaxed)
    }

    pub fn symbols_relayed(&self) -> u64 {
        self.symbols_relayed.load(Ordering::Relaxed)
    }

    pub async fn run(self: Arc<Self>, listener: TcpListener) -> anyhow::Result<()> {
        info!("orbit-relay listening on {}", listener.local_addr()?);
        let core = self;
        loop {
            let (socket, peer) = listener.accept().await?;
            debug!("accepted connection from {peer}");
            let core = core.clone();
            tokio::spawn(async move {
                if let Err(e) = core.handle_conn(socket).await {
                    debug!("connection handler error: {e}");
                }
            });
        }
    }

    async fn handle_conn(self: &Arc<Self>, socket: TcpStream) -> anyhow::Result<()> {
        let ws = tokio_tungstenite::accept_async(socket).await?;
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::channel::<WsMessage>(16384);

        let throttle = self.throttle_kbps;
        let sink_task = tokio::spawn(async move {
            let mut limiter = throttle.map(RateLimiter::new);
            while let Some(msg) = rx.recv().await {
                if let Some(limiter) = limiter.as_mut() {
                    limiter.throttle_bytes(msg.len()).await;
                }
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        let mut session_id: Option<u64> = None;
        let conn_id = uuid::Uuid::new_v4().to_string();

        while let Some(msg) = stream.next().await {
            let msg = match msg {
                Ok(msg) => msg,
                Err(_) => break,
            };
            match msg {
                WsMessage::Binary(frame) => {
                    let mut buf = BytesMut::from(&frame[..]);
                    let Some(parsed) = OrbitMessage::parse_frame(&mut buf)? else {
                        continue;
                    };
                    session_id = Some(parsed.session_id());
                    self.route(parsed, tx.clone(), conn_id.clone()).await;
                }
                WsMessage::Ping(p) => {
                    tx.send(WsMessage::Pong(p)).await?;
                }
                WsMessage::Close(_) => {
                    let _ = tx.try_send(WsMessage::Close(None));
                    break;
                }
                WsMessage::Pong(_) | WsMessage::Text(_) | WsMessage::Frame(_) => {}
            }
        }

        if let Some(id) = session_id {
            self.leave_room(id, &conn_id).await;
        }
        sink_task.abort();
        Ok(())
    }

    async fn route(
        self: &Arc<Self>,
        msg: OrbitMessage,
        tx: mpsc::Sender<WsMessage>,
        conn_id: String,
    ) {
        let id = msg.session_id();

        match msg {
            OrbitMessage::Hello { session_id, role } => {
                let (drained, receiver_tx, pending_direct) = {
                    let mut rooms = self.rooms.lock().await;
                    let room = rooms.entry(session_id).or_insert_with(Room::new);
                    match role {
                        ROLE_SENDER => {
                            if room.sender.is_some() {
                                warn!("session {session_id}: sender re-registered, dropping old link");
                            }
                            room.sender = Some((conn_id.clone(), tx));
                            let pending = room.pending_direct.take();
                            (Vec::new(), None, pending)
                        }
                        ROLE_RECEIVER => {
                            if room.receiver.is_some() {
                                warn!(
                                    "session {session_id}: receiver re-registered, dropping old link"
                                );
                            }
                            room.receiver = Some((conn_id.clone(), tx));
                            let drained = room.drain_buffer();
                            let rx = room.receiver.clone().map(|(_, r)| r);
                            (drained, rx, None)
                        }
                        other => {
                            warn!("unknown role byte {other}");
                            (Vec::new(), None, None)
                        }
                    }
                };
                info!("session {session_id}: role {role} registered");
                if let Some(rx) = receiver_tx {
                    for m in drained {
                        let _ = rx.send(m).await;
                    }
                }
                if let Some(frame) = pending_direct {
                    let sender_tx = {
                        let mut rooms = self.rooms.lock().await;
                        rooms
                            .get_mut(&session_id)
                            .and_then(|r| r.sender.clone().map(|(_, s)| s))
                    };
                    if let Some(sx) = sender_tx {
                        sx.send(frame).await.ok();
                    }
                }
            }
            OrbitMessage::Symbol {
                session_id,
                esi,
                data,
            } => {
                self.bytes_relayed.fetch_add(data.len() as u64, Ordering::Relaxed);
                self.symbols_relayed.fetch_add(1, Ordering::Relaxed);
                let frame = OrbitMessage::Symbol {
                    session_id,
                    esi,
                    data,
                }
                .encode()
                .freeze()
                .to_vec();
                let mut receiver_tx = {
                    let mut rooms = self.rooms.lock().await;
                    match rooms.get_mut(&session_id) {
                        Some(room) => {
                            if room.receiver_done {
                                None
                            } else {
                                match room.receiver.clone() {
                                    Some((_, rx)) => Some(rx),
                                    None => {
                                        room.push_buffer(WsMessage::Binary(frame.clone()));
                                        None
                                    }
                                }
                            }
                        }
                        None => {
                            let mut room = Room::new();
                            room.push_buffer(WsMessage::Binary(frame.clone()));
                            rooms.insert(session_id, room);
                            None
                        }
                    }
                };
                if let Some(rx) = receiver_tx.as_mut() {
                    // Backpressure, not drop: blocking the sender's handler
                    // propagates to TCP flow control, so a throttled relay
                    // never starves the receiver (try_send would drop every
                    // symbol once the channel is full).
                    let _ = rx.send(WsMessage::Binary(frame)).await;
                }
            }
            OrbitMessage::Meta { .. } => {
                let frame = msg.encode().freeze().to_vec();
                let mut receiver_tx = {
                    let mut rooms = self.rooms.lock().await;
                    match rooms.get_mut(&id) {
                        Some(room) => {
                            if room.receiver_done {
                                None
                            } else {
                                match room.receiver.clone() {
                                    Some((_, rx)) => Some(rx),
                                    None => {
                                        room.push_buffer(WsMessage::Binary(frame.clone()));
                                        None
                                    }
                                }
                            }
                        }
                        None => {
                            let mut room = Room::new();
                            room.push_buffer(WsMessage::Binary(frame.clone()));
                            rooms.insert(id, room);
                            None
                        }
                    }
                };
                if let Some(rx) = receiver_tx.as_mut() {
                    let _ = rx.send(WsMessage::Binary(frame)).await;
                }
            }
            OrbitMessage::Ready { .. } => {
                let frame = msg.encode().freeze().to_vec();
                let sender_tx = {
                    let mut rooms = self.rooms.lock().await;
                    match rooms.get_mut(&id) {
                        Some(room) => {
                            // The receiver is done: stop forwarding symbols
                            // to it. The receiver closes its own side and
                            // drains until our socket is closed, so it never
                            // closes with unread data (which would emit an
                            // RST and purge the in-flight READY).
                            room.receiver_done = true;
                            room.sender.clone().map(|(_, s)| s)
                        }
                        None => None,
                    }
                };
                match sender_tx {
                    Some(sx) => {
                        sx.send(WsMessage::Binary(frame)).await.ok();
                    }
                    None => {
                        warn!("session {id}: READY dropped, no sender registered");
                    }
                }
            }
            OrbitMessage::Direct { .. } => {
                let frame = msg.encode().freeze().to_vec();
                let sender_tx = {
                    let mut rooms = self.rooms.lock().await;
                    match rooms.get_mut(&id) {
                        Some(room) => match room.sender.clone() {
                            Some((_, s)) => Some(s),
                            None => {
                                // Sender not registered yet: keep it so it is
                                // flushed when the sender's Hello arrives.
                                room.pending_direct = Some(WsMessage::Binary(frame.clone()));
                                None
                            }
                        },
                        None => {
                            let mut room = Room::new();
                            room.pending_direct = Some(WsMessage::Binary(frame.clone()));
                            rooms.insert(id, room);
                            None
                        }
                    }
                };
                if let Some(sx) = sender_tx {
                    sx.send(WsMessage::Binary(frame)).await.ok();
                }
            }
            OrbitMessage::Done { .. } => {
                let mut rooms = self.rooms.lock().await;
                rooms.remove(&id);
            }
        }
    }

    async fn leave_room(&self, id: u64, conn_id: &str) {
        let mut rooms = self.rooms.lock().await;
        if let Some(room) = rooms.get_mut(&id) {
            if room
                .sender
                .as_ref()
                .map(|(cid, _)| cid == conn_id)
                .unwrap_or(false)
            {
                room.sender = None;
            }
            if room
                .receiver
                .as_ref()
                .map(|(cid, _)| cid == conn_id)
                .unwrap_or(false)
            {
                room.receiver = None;
            }
            if room.sender.is_none() && room.receiver.is_none() && room.buffer.is_empty() {
                rooms.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use orbit_protocol::wire::ROLE_RECEIVER;
    use std::time::Duration;

    async fn relayed_kind(rx: &mut mpsc::Receiver<WsMessage>) -> Option<&'static str> {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(WsMessage::Binary(frame))) => {
                let mut buf = BytesMut::from(&frame[..]);
                let msg = OrbitMessage::parse_frame(&mut buf).ok()??;
                Some(relay_kind(&msg))
            }
            _ => None,
        }
    }

    fn relay_kind(msg: &OrbitMessage) -> &'static str {
        match msg {
            OrbitMessage::Hello { .. } => "Hello",
            OrbitMessage::Meta { .. } => "Meta",
            OrbitMessage::Symbol { .. } => "Symbol",
            OrbitMessage::Done { .. } => "Done",
            OrbitMessage::Ready { .. } => "Ready",
            OrbitMessage::Direct { .. } => "Direct",
        }
    }

    fn symbol(id: u64, esi: u32) -> OrbitMessage {
        OrbitMessage::Symbol {
            session_id: id,
            esi,
            data: Bytes::from(vec![0xAB; 32]),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buffers_symbols_until_receiver_registers() -> anyhow::Result<()> {
        let core = Arc::new(RelayCore::new());
        let (sender_tx, _sender_rx) = mpsc::channel(16);
        let (recv_tx, mut recv_rx) = mpsc::channel::<WsMessage>(16);

        core.route(
            OrbitMessage::Hello {
                session_id: 1,
                role: ROLE_SENDER,
            },
            sender_tx.clone(),
            "s1".into(),
        )
        .await;
        for esi in 0..3u32 {
            core.route(symbol(1, esi), sender_tx.clone(), "s1".into())
                .await;
        }
        // Receiver joins late: buffered symbols must be flushed in order.
        core.route(
            OrbitMessage::Hello {
                session_id: 1,
                role: ROLE_RECEIVER,
            },
            recv_tx,
            "r1".into(),
        )
        .await;
        for esi in 0..3u32 {
            let got = relayed_kind(&mut recv_rx).await;
            assert_eq!(got, Some("Symbol"), "buffered symbol {esi} must be flushed");
        }
        assert!(
            relayed_kind(&mut recv_rx).await.is_none(),
            "no extra message must be queued"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forwards_ready_and_stops_symbols_after_ready() -> anyhow::Result<()> {
        let core = Arc::new(RelayCore::new());
        let (sender_tx, mut sender_rx) = mpsc::channel::<WsMessage>(16);
        let (recv_tx, mut recv_rx) = mpsc::channel::<WsMessage>(16);

        core.route(
            OrbitMessage::Hello {
                session_id: 7,
                role: ROLE_SENDER,
            },
            sender_tx.clone(),
            "s1".into(),
        )
        .await;
        core.route(
            OrbitMessage::Hello {
                session_id: 7,
                role: ROLE_RECEIVER,
            },
            recv_tx.clone(),
            "r1".into(),
        )
        .await;
        core.route(symbol(7, 0), sender_tx.clone(), "s1".into())
            .await;
        assert_eq!(relayed_kind(&mut recv_rx).await, Some("Symbol"));

        core.route(
            OrbitMessage::Ready { session_id: 7 },
            recv_tx.clone(),
            "r1".into(),
        )
        .await;
        assert_eq!(relayed_kind(&mut sender_rx).await, Some("Ready"));

        // After READY the receiver is done: further symbols must be dropped.
        core.route(symbol(7, 1), sender_tx.clone(), "s1".into())
            .await;
        assert!(
            relayed_kind(&mut recv_rx).await.is_none(),
            "symbols must not be forwarded after READY"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn keeps_direct_advertisement_until_sender_registers() -> anyhow::Result<()> {
        let core = Arc::new(RelayCore::new());
        let (sender_tx, mut sender_rx) = mpsc::channel::<WsMessage>(16);
        let (recv_tx, _recv_rx) = mpsc::channel::<WsMessage>(16);

        // Receiver advertises its P2P address before the sender joins.
        core.route(
            OrbitMessage::Hello {
                session_id: 3,
                role: ROLE_RECEIVER,
            },
            recv_tx.clone(),
            "r1".into(),
        )
        .await;
        core.route(
            OrbitMessage::Direct {
                session_id: 3,
                addr: "127.0.0.1:9999".into(),
            },
            recv_tx.clone(),
            "r1".into(),
        )
        .await;

        // The sender must receive the pending advertisement on registration.
        core.route(
            OrbitMessage::Hello {
                session_id: 3,
                role: ROLE_SENDER,
            },
            sender_tx.clone(),
            "s1".into(),
        )
        .await;
        assert_eq!(relayed_kind(&mut sender_rx).await, Some("Direct"));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn throttle_accuracy() {
        // 2000 kbps = 2,048,000 B/s; 4105 B per symbol → ~499 symbols/s.
        let mut limiter = RateLimiter::new(2000);
        let n = 1000usize;
        let start = std::time::Instant::now();
        for _ in 0..n {
            limiter.throttle_bytes(4105).await;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let expected = n as f64 * 4105.0 / 2048000.0;
        let actual = n as f64 * 4105.0 / elapsed;
        eprintln!(
            "throttle: {n} x 4105 B in {elapsed:.2}s (expected {expected:.2}s) -> {actual:.0} B/s ({:.2} MiB/s)",
            actual / 1048576.0
        );
        assert!(
            (elapsed - expected).abs() < expected * 0.5,
            "limiter drift too large: {elapsed:.2}s vs {expected:.2}s"
        );
    }
}