use bytes::BytesMut;
use orbit_protocol::wire::OrbitMessage;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// Direct P2P channel: a raw TCP link carrying the same binary OrbitMessage
/// framing, bypassing the relay entirely.
pub struct DirectChannel {
    writer: OwnedWriteHalf,
    reader: OwnedReadHalf,
}

/// Pipelined writer half: symbols are queued and drained by a background
/// task, so the emission loop never blocks on the link.
pub struct DirectWriter {
    tx: mpsc::Sender<OrbitMessage>,
    alive: Arc<AtomicBool>,
}

const DIRECT_QUEUE_DEPTH: usize = 1024;

/// Binds a TCP listener for the receiving side of a P2P session.
pub struct DirectListener {
    listener: TcpListener,
}

impl DirectListener {
    pub async fn bind(addr: &str) -> anyhow::Result<(SocketAddr, Self)> {
        let listener = TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        Ok((local, Self { listener }))
    }

    pub async fn accept(&mut self) -> anyhow::Result<DirectChannel> {
        let (stream, _) = self.listener.accept().await?;
        let (reader, writer) = stream.into_split();
        Ok(DirectChannel { writer, reader })
    }
}

/// Reads complete OrbitMessage frames from any byte stream.
async fn recv_frames<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<Option<OrbitMessage>> {
    let mut buf = BytesMut::with_capacity(64 * 1024);
    loop {
        // Try to parse a complete frame from what we have.
        if buf.len() >= 5 {
            if let Some(msg) = OrbitMessage::parse_frame(&mut buf)? {
                return Ok(Some(msg));
            }
        }
        // Need more bytes: read one chunk.
        let mut chunk = [0u8; 64 * 1024];
        let n = match reader.read(&mut chunk).await {
            Ok(0) => return Ok(None),
            Ok(n) => n,
            Err(e) => return Err(e.into()),
        };
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Spawns the background drain task shared by every transport's
/// `into_pipeline`. Returns a [`DirectWriter`] plus the read half back to
/// the caller.
fn spawn_writer<W>(writer: W, reader_ret: ReaderHalf) -> (DirectWriter, ReaderHalf)
where
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (tx, mut rx) = mpsc::channel::<OrbitMessage>(DIRECT_QUEUE_DEPTH);
    let alive = Arc::new(AtomicBool::new(true));
    let alive_task = alive.clone();
    let mut writer = writer;
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if writer.write_all(&msg.encode()).await.is_err() || writer.flush().await.is_err() {
                alive_task.store(false, Ordering::SeqCst);
                return;
            }
        }
    });
    (DirectWriter { tx, alive }, reader_ret)
}

/// Read half of a direct P2P channel, kept only so `into_pipeline` can be
/// symmetric; the send side never reads on the direct path.
pub enum ReaderHalf {
    Tcp(OwnedReadHalf),
    Quic(quinn::RecvStream),
}

impl DirectChannel {
    pub async fn connect(addr: &str) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        Ok(Self { writer, reader })
    }

    pub async fn send(&mut self, msg: &OrbitMessage) -> anyhow::Result<()> {
        let frame = msg.encode();
        self.writer.write_all(&frame).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Splits into a pipelined writer (background drain task) plus the
    /// read half, so producers never block on the socket.
    pub fn into_pipeline(self) -> (DirectWriter, ReaderHalf) {
        spawn_writer(self.writer, ReaderHalf::Tcp(self.reader))
    }

    pub async fn recv(&mut self) -> anyhow::Result<Option<OrbitMessage>> {
        recv_frames(&mut self.reader).await
    }
}

/// QUIC direct P2P channel: the same binary OrbitMessage framing over a
/// QUIC bidirectional stream instead of TCP. Self-signed TLS, so the peer
/// is expected to trust it implicitly (the symbol payloads are additionally
/// sealed by `orbit-crypto` when `--secret` is used).
///
/// The `connection` and `endpoint` must be kept alive for as long as the
/// stream is used: dropping the `Connection` handle closes it, and dropping
/// the `Endpoint` stops the UDP socket driver.
pub struct QuicChannel {
    writer: quinn::SendStream,
    reader: quinn::RecvStream,
    connection: quinn::Connection,
    endpoint: quinn::Endpoint,
}

/// Binds a QUIC endpoint for the receiving side of a P2P session.
pub struct QuicListener {
    endpoint: quinn::Endpoint,
}

impl QuicListener {
    pub async fn bind(addr: &str) -> anyhow::Result<(SocketAddr, Self)> {
        let addr = addr.strip_prefix("quic://").unwrap_or(addr);
        let endpoint = quinn::Endpoint::server(quic_server_config()?, addr.parse()?)?;
        let local = endpoint.local_addr()?;
        Ok((local, Self { endpoint }))
    }

    pub async fn accept(&mut self) -> anyhow::Result<QuicChannel> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| anyhow::anyhow!("QUIC endpoint closed"))?;
        let connection = incoming.await?;
        let (writer, reader) = connection.accept_bi().await?;
        Ok(QuicChannel {
            writer,
            reader,
            connection,
            endpoint: self.endpoint.clone(),
        })
    }
}

impl QuicChannel {
    pub async fn connect(addr: &str) -> anyhow::Result<Self> {
        let addr = addr.strip_prefix("quic://").unwrap_or(addr);
        let addr: SocketAddr = addr.parse()?;
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(quic_client_config());
        let connection = endpoint.connect(addr, "localhost")?.await?;
        let (writer, reader) = connection.open_bi().await?;
        Ok(Self {
            writer,
            reader,
            connection,
            endpoint,
        })
    }

    pub async fn send(&mut self, msg: &OrbitMessage) -> anyhow::Result<()> {
        let frame = msg.encode();
        self.writer.write_all(&frame).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Splits into a pipelined writer; the read half is retained only for
    /// symmetry (the send side never reads on the direct path). The
    /// connection and endpoint move into the drain task so the QUIC session
    /// stays alive while symbols stream out.
    pub fn into_pipeline(self) -> (DirectWriter, ReaderHalf) {
        let QuicChannel {
            writer,
            reader,
            connection,
            endpoint,
        } = self;
        let (tx, mut rx) = mpsc::channel::<OrbitMessage>(DIRECT_QUEUE_DEPTH);
        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();
        let mut writer = writer;
        tokio::spawn(async move {
            let _keep_alive = (connection, endpoint);
            while let Some(msg) = rx.recv().await {
                if writer.write_all(&msg.encode()).await.is_err() || writer.flush().await.is_err() {
                    alive_task.store(false, Ordering::SeqCst);
                    return;
                }
            }
        });
        (DirectWriter { tx, alive }, ReaderHalf::Quic(reader))
    }

    pub async fn recv(&mut self) -> anyhow::Result<Option<OrbitMessage>> {
        recv_frames(&mut self.reader).await
    }
}

/// A direct P2P channel over either TCP or QUIC, selected by the advertised
/// address: `quic://host:port` uses QUIC, anything else uses TCP.
pub enum P2pChannel {
    Tcp(DirectChannel),
    Quic(QuicChannel),
}

impl P2pChannel {
    pub async fn connect(addr: &str) -> anyhow::Result<Self> {
        if addr.starts_with("quic://") {
            Ok(Self::Quic(QuicChannel::connect(addr).await?))
        } else {
            Ok(Self::Tcp(DirectChannel::connect(addr).await?))
        }
    }

    pub async fn send(&mut self, msg: &OrbitMessage) -> anyhow::Result<()> {
        match self {
            Self::Tcp(c) => c.send(msg).await,
            Self::Quic(c) => c.send(msg).await,
        }
    }

    pub fn into_pipeline(self) -> (DirectWriter, ReaderHalf) {
        match self {
            Self::Tcp(c) => c.into_pipeline(),
            Self::Quic(c) => c.into_pipeline(),
        }
    }

    pub async fn recv(&mut self) -> anyhow::Result<Option<OrbitMessage>> {
        match self {
            Self::Tcp(c) => c.recv().await,
            Self::Quic(c) => c.recv().await,
        }
    }
}

/// A P2P listener over either TCP or QUIC. The address is advertised by the
/// receiver via the relay's `Direct` message.
pub enum P2pListener {
    Tcp(DirectListener),
    Quic(QuicListener),
}

impl P2pListener {
    pub async fn bind(addr: &str) -> anyhow::Result<(SocketAddr, Self)> {
        if addr.starts_with("quic://") {
            let (local, listener) = QuicListener::bind(addr).await?;
            Ok((local, Self::Quic(listener)))
        } else {
            let (local, listener) = DirectListener::bind(addr).await?;
            Ok((local, Self::Tcp(listener)))
        }
    }

    pub async fn accept(&mut self) -> anyhow::Result<P2pChannel> {
        match self {
            Self::Tcp(l) => Ok(P2pChannel::Tcp(l.accept().await?)),
            Self::Quic(l) => Ok(P2pChannel::Quic(l.accept().await?)),
        }
    }
}

/// Accepts any server certificate. QUIC is layered under the session's
/// AEAD encryption, so pinning a self-signed P2P certificate would add no
/// security over a random one.
#[derive(Debug)]
struct NoVerifier;

impl quinn::rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &quinn::rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[quinn::rustls::pki_types::CertificateDer<'_>],
        _server_name: &quinn::rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: quinn::rustls::pki_types::UnixTime,
    ) -> Result<quinn::rustls::client::danger::ServerCertVerified, quinn::rustls::Error> {
        Ok(quinn::rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &quinn::rustls::pki_types::CertificateDer<'_>,
        _dss: &quinn::rustls::DigitallySignedStruct,
    ) -> Result<quinn::rustls::client::danger::HandshakeSignatureValid, quinn::rustls::Error> {
        Ok(quinn::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &quinn::rustls::pki_types::CertificateDer<'_>,
        _dss: &quinn::rustls::DigitallySignedStruct,
    ) -> Result<quinn::rustls::client::danger::HandshakeSignatureValid, quinn::rustls::Error> {
        Ok(quinn::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<quinn::rustls::SignatureScheme> {
        vec![
            quinn::rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            quinn::rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            quinn::rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            quinn::rustls::SignatureScheme::RSA_PSS_SHA256,
            quinn::rustls::SignatureScheme::RSA_PSS_SHA384,
            quinn::rustls::SignatureScheme::RSA_PSS_SHA512,
            quinn::rustls::SignatureScheme::RSA_PKCS1_SHA256,
            quinn::rustls::SignatureScheme::RSA_PKCS1_SHA384,
            quinn::rustls::SignatureScheme::RSA_PKCS1_SHA512,
            quinn::rustls::SignatureScheme::ED25519,
        ]
    }
}

fn quic_client_config() -> quinn::ClientConfig {
    let rustls_client = quinn::rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();
    quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(rustls_client)
            .expect("valid QUIC rustls client config"),
    ))
}

fn quic_server_config() -> anyhow::Result<quinn::ServerConfig> {
    let self_signed = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let cert = self_signed.cert.der().clone();
    let key = quinn::rustls::pki_types::PrivateKeyDer::Pkcs8(
        quinn::rustls::pki_types::PrivatePkcs8KeyDer::from(self_signed.signing_key.serialize_der()),
    );
    Ok(quinn::ServerConfig::with_single_cert(vec![cert], key)?)
}

impl DirectWriter {
    /// Queues a message; returns `false` when the queue is full or the
    /// link is dead (never blocks).
    pub fn try_send(&self, msg: &OrbitMessage) -> bool {
        if !self.alive.load(Ordering::SeqCst) {
            return false;
        }
        self.tx.try_send(msg.clone()).is_ok()
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::wire::ROLE_SENDER;
    use tokio::net::TcpListener;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn direct_roundtrip() -> anyhow::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut ch = DirectChannel { writer, reader };
            let msg = ch.recv().await.unwrap().unwrap();
            assert_eq!(
                msg,
                OrbitMessage::Hello {
                    session_id: 1,
                    role: ROLE_SENDER
                }
            );
        });

        let mut client = DirectChannel::connect(&addr.to_string()).await?;
        client
            .send(&OrbitMessage::Hello {
                session_id: 1,
                role: ROLE_SENDER,
            })
            .await?;
        server.await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_roundtrip() -> anyhow::Result<()> {
        let (addr, mut listener) = P2pListener::bind("quic://127.0.0.1:0").await?;
        let server = tokio::spawn(async move {
            let mut ch = listener.accept().await.unwrap();
            let msg = ch.recv().await.unwrap().unwrap();
            assert_eq!(
                msg,
                OrbitMessage::Hello {
                    session_id: 7,
                    role: ROLE_SENDER
                }
            );
        });

        let addr = format!("quic://{addr}");
        let mut client = P2pChannel::connect(&addr).await?;
        client
            .send(&OrbitMessage::Hello {
                session_id: 7,
                role: ROLE_SENDER,
            })
            .await?;
        server.await?;
        Ok(())
    }
}