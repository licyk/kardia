use std::sync::Arc;

use kardia_core::{Context, Inject, Plugin, Result};
use kardia_server::{Server, ServerExt, WsExt, ws_handler};
use serde_json::{Value, json};

pub struct EchoPlugin;
impl Plugin for EchoPlugin {
    const NAME: &'static str = "server:echo";
    type Config = Value;
    fn inject() -> Inject {
        Inject::new().with("std:server")
    }
    async fn apply(ctx: Context, _: Arc<Value>) -> Result<()> {
        let server = ctx.get::<Server>()?;
        server.post("", |req, res, _| async move {
            if let Some(content_type) = req.headers.get("content-type").filter(|v| !v.is_empty()) {
                res.set_header(kardia_server::HeaderName::from_static("content-type"), content_type.clone());
            }
            res.set_body(req.take_body()?);
            Ok(None)
        })?;
        server.ws(
            "",
            Some(ws_handler(|_, accept| async move {
                let socket = accept.accept().await?;
                while let Some(message) = socket.recv().await {
                    let message = message?;
                    if message.is_close() {
                        break;
                    }
                    socket.send(message)?;
                }
                Ok(())
            })),
            json!({}),
        )?;
        Ok(())
    }
}
