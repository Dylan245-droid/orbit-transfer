use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use orbit_protocol::wire::OrbitMessage;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsSink = futures_util::stream::SplitSink<
    WebSocketStream<MaybeTlsStream<TcpStream>>,
    WsMessage,
>;
type WsStream = futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

/// Outbound half of a relay connection.
///
/// Internally a shared mutex, so background tasks (P2P re-advertisement)
/// can share the same WebSocket without interleaving frames.
#[derive(Clone)]
pub struct RelaySink {
    sink: Arc<tokio::sync::Mutex<WsSink>>,
}

/// Inbound half of a relay connection.
pub struct RelayStream {
    stream: WsStream,
}

impl RelaySink {
    pub async fn send(&self, msg: &OrbitMessage) -> anyhow::Result<()> {
        let frame = msg.encode().freeze();
        self.sink
            .lock()
            .await
            .send(WsMessage::Binary(frame.to_vec()))
            .await?;
        Ok(())
    }

    /// Flushes a WebSocket Close frame without waiting for the peer's
    /// reply. Used by a receiver that has finished decoding: it must keep
    /// reading (draining) until the relay's socket is fully closed, so its
    /// own socket is never closed with unread data (which would emit an RST
    /// and purge the in-flight READY at the relay).
    pub async fn send_close(&self) -> anyhow::Result<()> {
        self.sink
            .lock()
            .await
            .send(WsMessage::Close(None))
            .await?;
        Ok(())
    }
}

impl RelayStream {
    /// Consumes everything the relay sends until its socket is fully closed.
    pub async fn drain_until_closed(&mut self) -> anyhow::Result<()> {
        loop {
            match self.stream.next().await {
                Some(Ok(WsMessage::Close(_))) => continue,
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            }
        }
    }
}

impl RelayStream {
    pub async fn recv(&mut self) -> anyhow::Result<Option<OrbitMessage>> {
        loop {
            match self.stream.next().await {
                Some(Ok(WsMessage::Binary(frame))) => {
                    let mut buf = BytesMut::from(&frame[..]);
                    return Ok(OrbitMessage::parse_frame(&mut buf)?);
                }
                Some(Ok(WsMessage::Close(_))) | None => return Ok(None),
                Some(Ok(WsMessage::Ping(_)))
                | Some(Ok(WsMessage::Pong(_)))
                | Some(Ok(WsMessage::Text(_)))
                | Some(Ok(WsMessage::Frame(_))) => continue,
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}

/// Opens a WebSocket connection to a relay and returns the two halves.
pub async fn connect(url: &str) -> anyhow::Result<(RelaySink, RelayStream)> {
    let (ws, _resp) = connect_async(url).await?;
    let (sink, stream) = ws.split();
    Ok((
        RelaySink {
            sink: Arc::new(tokio::sync::Mutex::new(sink)),
        },
        RelayStream { stream },
    ))
}