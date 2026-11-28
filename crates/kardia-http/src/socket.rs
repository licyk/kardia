use crate::{Http, RequestConfig, body::Operation, service::Active};
use futures::{FutureExt, SinkExt, StreamExt};
use kardia_core::{Disposer, Error, Result, Svc};
use std::{panic::AssertUnwindSafe, sync::Arc};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::derive_accept_key,
        protocol::{CloseFrame, Role, frame::coding::CloseCode},
    },
};

#[derive(Clone)]
pub struct ClientSocket {
    outgoing: mpsc::UnboundedSender<Message>,
    incoming: Arc<Mutex<mpsc::UnboundedReceiver<Result<Message>>>>,
    _operation: Arc<Operation>,
}
impl ClientSocket {
    pub fn send(&self, message: Message) -> Result<()> {
        self.outgoing.send(message).map_err(|_| Error::msg("WebSocket is closed"))
    }
    pub async fn recv(&self) -> Option<Result<Message>> {
        self.incoming.lock().await.recv().await
    }
    pub fn close(&self, frame: Option<CloseFrame>) -> Result<()> {
        self.send(Message::Close(frame))
    }
}
pub trait WebSocketExt {
    fn ws(&self, url: &str, config: RequestConfig) -> impl Future<Output = Result<ClientSocket>> + Send;
}
impl WebSocketExt for Svc<Http> {
    async fn ws(&self, url: &str, config: RequestConfig) -> Result<ClientSocket> {
        let mut request = config.resolve(self, url)?;
        let scheme = match request.url.scheme() {
            "ws" | "http" => "http",
            "wss" | "https" => "https",
            _ => return Err(Error::msg("invalid WebSocket URL")),
        };
        request.url.set_scheme(scheme).map_err(|_| Error::msg("invalid WebSocket URL"))?;
        let client = self.client(request.proxy_agent.as_deref(), &request.url, &request.no_proxy)?;
        let protocols = request
            .headers
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').map(|p| p.trim().to_owned()).collect::<Vec<_>>())
            .unwrap_or_default();
        let key = tokio_tungstenite::tungstenite::handshake::client::generate_key();
        for (name, value) in [
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", key.as_str()),
        ] {
            request.headers.insert(http::HeaderName::from_bytes(name.as_bytes())?, value.parse()?);
        }
        let (send, rx) = mpsc::unbounded_channel();
        let (tx, incoming) = mpsc::unbounded_channel();
        let (ready, wait) = oneshot::channel();
        let (stop, closed) = watch::channel(false);
        let mut provider = self.stop.subscribe();
        let active = Active::new(&self.active);
        let operation = async move {
            let _active = active;
            let work = async {
                let mut cancelled = closed.clone();
                let response = tokio::select! {
                    _=cancelled.wait_for(|v|*v).map(|_|())=>return Err(Error::msg("WebSocket connection cancelled")),
                    response=client.get(request.url).headers(request.headers).send()=>response?,
                };
                if response.status() != 101
                    || !response
                        .headers()
                        .get("upgrade")
                        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
                    || !response
                        .headers()
                        .get("connection")
                        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"upgrade"))
                    || response.headers().get("sec-websocket-accept").and_then(|v| v.to_str().ok())
                        != Some(derive_accept_key(key.as_bytes()).as_str())
                {
                    return Err(Error::msg("WebSocket upgrade rejected"));
                }
                if response
                    .headers()
                    .get("sec-websocket-protocol")
                    .is_some_and(|v| !protocols.iter().any(|p| v.as_bytes() == p.as_bytes()))
                {
                    return Err(Error::msg("WebSocket server selected an unrequested protocol"));
                }
                let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(None)
                    .max_frame_size(None);
                let stream =
                    WebSocketStream::from_raw_socket(response.upgrade().await?, Role::Client, Some(config)).await;
                let _ = ready.send(());
                drive(stream, rx, &tx, closed).await
            };
            let result = tokio::select! {_ = provider.wait_for(|v|*v).map(|_|())=>Err(Error::msg("HTTP provider disposed")),result=AssertUnwindSafe(work).catch_unwind()=>result.unwrap_or_else(|p|Err(Error::from_panic(p)))};
            if let Err(error) = result {
                let _ = tx.send(Err(error));
            }
        };
        let effect = self.caller().effect_sync("http WebSocket", |scope| {
            let task = tokio::spawn(operation);
            scope.defer(Disposer::future(move || {
                stop.send_replace(true);
                async move {
                    let mut task = task;
                    if tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await.is_err() {
                        task.abort();
                        let _ = task.await;
                    }
                }
            }));
            Ok(())
        })?;
        let operation = Arc::new(Operation(effect));
        let incoming = Arc::new(Mutex::new(incoming));
        if wait.await.is_err() {
            return Err(incoming
                .lock()
                .await
                .recv()
                .await
                .and_then(Result::err)
                .unwrap_or_else(|| Error::msg("WebSocket connection cancelled")));
        }
        Ok(ClientSocket { outgoing: send, incoming, _operation: operation })
    }
}
async fn drive(
    mut socket: WebSocketStream<reqwest::Upgraded>,
    mut outgoing: mpsc::UnboundedReceiver<Message>,
    incoming: &mpsc::UnboundedSender<Result<Message>>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            _=stop.wait_for(|v|*v).map(|_|())=>{let _=socket.send(Message::Close(Some(CloseFrame{code:CloseCode::Normal,reason:"context disposed".into()}))).await;return Ok(())},
            message=outgoing.recv()=>match message{Some(message)=>{let close=message.is_close();socket.send(message).await?;if close{return Ok(())}},None=>return Ok(())},
            message=socket.next()=>match message{Some(Ok(message))=>{if message.is_ping()||message.is_pong(){socket.flush().await?;continue}let close=message.is_close();let _=incoming.send(Ok(message));if close{let _=socket.flush().await;return Ok(())}},Some(Err(error))=>return Err(error.into()),None=>return Ok(())},
        }
    }
}
