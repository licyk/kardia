use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use kardia_core::{Error, Result};
use tokio::sync::{Mutex, mpsc, watch};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

/// An accepted connection. Returning from a handler does not close it;
/// the calling route owns its lifetime.
#[derive(Clone)]
pub struct WebSocket {
    outgoing: mpsc::UnboundedSender<Message>,
    incoming: Arc<Mutex<mpsc::UnboundedReceiver<Result<Message>>>>,
}
impl WebSocket {
    pub fn send(&self, message: Message) -> Result<()> {
        self.outgoing.send(message).map_err(|_| Error::msg("WebSocket is closed"))
    }
    pub async fn recv(&self) -> Option<Result<Message>> {
        self.incoming.lock().await.recv().await
    }
    pub fn close(&self, frame: Option<crate::CloseFrame>) -> Result<()> {
        self.send(Message::Close(frame))
    }
}

type Stream = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

pub(crate) fn channels(
    stream: Stream,
    route: watch::Receiver<bool>,
    connection: watch::Receiver<bool>,
    count: Arc<AtomicUsize>,
) -> (WebSocket, impl Future<Output = Result<()>> + Send) {
    let (outgoing, rx) = mpsc::unbounded_channel();
    let (tx, incoming) = mpsc::unbounded_channel();
    let socket = WebSocket { outgoing, incoming: Arc::new(Mutex::new(incoming)) };
    count.fetch_add(1, Ordering::SeqCst);
    let guard = ClientCount(count);
    (socket, async move {
        let _guard = guard;
        drive(stream, route, connection, rx, tx).await
    })
}

struct ClientCount(Arc<AtomicUsize>);
impl Drop for ClientCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn drive(
    mut stream: Stream,
    mut route: watch::Receiver<bool>,
    mut connection: watch::Receiver<bool>,
    mut outgoing: mpsc::UnboundedReceiver<Message>,
    incoming: mpsc::UnboundedSender<Result<Message>>,
) -> Result<()> {
    let mut outgoing_open = true;
    let mut connection_open = true;
    loop {
        tokio::select! {
            biased;
            _ = route.changed() => { let _=stream.send(Message::Close(None)).await; return Ok(()) }
            changed = connection.changed(), if connection_open => {
                if changed.is_ok() { let _=stream.send(Message::Close(None)).await; return Ok(()) }
                connection_open=false;
            }
            message = outgoing.recv(), if outgoing_open => match message {
                Some(message) => { let close=message.is_close(); stream.send(message).await?; if close { return Ok(()) } }
                None => outgoing_open=false,
            },
            message = stream.next() => match message {
                Some(Ok(message)) => {
                    if message.is_ping() || message.is_pong() { stream.flush().await?; continue }
                    let close=message.is_close(); let _=incoming.send(Ok(message));
                    if close { let _=stream.flush().await; return Ok(()) }
                }
                Some(Err(error)) => { let error:Error=error.into(); let _=incoming.send(Err(error.clone())); return Err(error) }
                None => return Ok(()),
            }
        }
    }
}
