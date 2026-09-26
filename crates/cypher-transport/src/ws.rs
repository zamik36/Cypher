//! WebSocket framing for browsers: one binary message per wire frame.

use std::io;

use bytes::Bytes;
use cypher_types::{Error, MAX_FRAME_SIZE, Result};
use futures::{SinkExt, StreamExt, future};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::{FrameSink, FrameStream, HANDSHAKE_TIMEOUT};

pub async fn accept_ws(tcp: TcpStream) -> Result<(FrameStream, FrameSink)> {
    tcp.set_nodelay(true)?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_SIZE))
        .max_frame_size(Some(MAX_FRAME_SIZE));
    let ws = timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::accept_async_with_config(tcp, Some(config)),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(|e| Error::Transport(e.to_string()))?;
    let (sink, stream) = ws.split();
    let stream = stream.filter_map(|m| {
        future::ready(match m {
            Ok(Message::Binary(b)) => Some(Ok(b)),
            Ok(Message::Close(_)) | Err(_) => Some(Err(io::ErrorKind::ConnectionAborted.into())),
            Ok(_) => None,
        })
    });
    let sink = sink
        .sink_map_err(|e| io::Error::other(e.to_string()))
        .with(|b: Bytes| future::ready(Ok::<_, io::Error>(Message::Binary(b))));
    Ok((Box::pin(stream), Box::pin(sink)))
}
