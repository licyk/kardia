use kardia_core::Result;
use kardia_http::{ClientSocket, CloseFrame, Message};
use kardia_server::WebSocket;

pub(crate) async fn forward(upstream: ClientSocket, downstream: WebSocket) -> Result<()> {
    loop {
        tokio::select! {
            message = downstream.recv() => match message {
                Some(Ok(Message::Close(frame))) => { let _ = upstream.close(Some(sanitize(frame))); while upstream.recv().await.is_some() {} return Ok(()) }
                Some(Ok(message)) => { upstream.send(message)?; }
                Some(Err(_)) | None => { let _ = upstream.close(Some(sanitize(None))); while upstream.recv().await.is_some() {} return Ok(()) }
            },
            message = upstream.recv() => match message {
                Some(Ok(Message::Close(frame))) => { let _ = downstream.close(Some(sanitize(frame))); return Ok(()) }
                Some(Ok(message)) => { downstream.send(message)?; }
                Some(Err(_)) => { let _ = downstream.close(Some(CloseFrame { code: 1011.into(), reason: "upstream error".into() })); return Ok(()) }
                None => { let _ = downstream.close(Some(sanitize(None))); return Ok(()) }
            },
        }
    }
}
fn sanitize(frame: Option<CloseFrame>) -> CloseFrame {
    let mut frame = frame.unwrap_or(CloseFrame { code: 1000.into(), reason: "".into() });
    let code: u16 = frame.code.into();
    if code != 1000 && !(3000..5000).contains(&code) {
        frame.code = 1000.into();
    }
    frame
}
