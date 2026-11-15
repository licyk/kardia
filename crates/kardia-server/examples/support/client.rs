use kardia_core::{Context, Result};
use kardia_http::{Body, Http, HttpConfig, HttpExt, HttpPlugin, Redirect, RequestConfig, WebSocketExt};
use kardia_server::{Server, ServerExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub async fn install(ctx: &Context) -> Result<()> {
    let config = std::env::var("KARDIA_HTTP_CONFIG").unwrap_or_else(|_| "{}".into());
    ctx.plugin::<HttpPlugin>(serde_json::from_str::<HttpConfig>(&config)?)?.settled().await?;
    let caller = ctx.clone();
    ctx.get::<Server>()?.post("/request", move |req, res, _| {
        let ctx = caller.clone();
        async move {
            let args: Args = serde_json::from_value(req.json().await?)?;
            let http = ctx.intercept("http", args.intercept.clone()).get::<Http>()?;
            match http.request(&args.url, args.config()?).await {
                Ok(response) => {
                    let status = response.status.as_u16();
                    let reason = response.status_text;
                    let headers = serde_json::to_value(
                        ["content-type", "content-encoding", "content-length", "x-custom", "location"]
                            .into_iter()
                            .map(|name| {
                                (name, response.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned))
                            })
                            .collect::<BTreeMap<_, _>>(),
                    )?;
                    match response.body.bytes().await {
                        Ok(body) => res.set_json(
                            &json!({"status":status,"reason":reason,"headers":headers,"body":body.to_vec()}),
                        )?,
                        Err(_) => res.set_json(&json!({"error":true,"timeout":false}))?,
                    }
                }
                Err(error) => res.set_json(&json!({"error":true,"timeout":error.is::<kardia_http::HttpError>()}))?,
            }
            Ok(None)
        }
    })?;
    let caller = ctx.clone();
    ctx.get::<Server>()?.post("/socket", move |req, res, _| {
        let ctx = caller.clone();
        async move {
            let args: Args = serde_json::from_value(req.json().await?)?;
            let http = ctx.intercept("http", args.intercept.clone()).get::<Http>()?;
            let result = async {
                let socket = http.ws(&args.url, args.config()?).await?;
                let mut messages = Vec::new();
                for message in
                    [kardia_http::Message::Text("hello 中文".into()), kardia_http::Message::Binary(vec![0, 255].into())]
                {
                    socket.send(message)?;
                    let message = socket.recv().await.ok_or_else(|| kardia_core::Error::msg("closed"))??;
                    messages.push(
                        json!({"type":if message.is_text(){"text"}else{"binary"},"bytes":message.into_data().to_vec()}),
                    );
                }
                socket.close(None)?;
                Ok::<_, kardia_core::Error>(messages)
            }
            .await;
            match result {
                Ok(messages) => res.set_json(&json!({"messages":messages}))?,
                Err(_) => res.set_json(&json!({"error":true}))?,
            };
            Ok(None)
        }
    })?;
    Ok(())
}
#[derive(Deserialize)]
struct Args {
    url: String,
    method: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    data: Option<String>,
    timeout: Option<u64>,
    redirect: Option<String>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    intercept: Value,
}
impl Args {
    fn config(&self) -> Result<RequestConfig> {
        let headers = self
            .headers
            .as_ref()
            .map(|values| {
                let mut headers = http::HeaderMap::new();
                for (k, v) in values {
                    headers.insert(http::HeaderName::from_bytes(k.as_bytes())?, v.parse()?);
                }
                Ok::<_, kardia_core::Error>(headers)
            })
            .transpose()?;
        let body = self.data.as_ref().map(|value| {
            let bytes = kardia_http::Bytes::copy_from_slice(value.as_bytes());
            if self.stream { Body::stream(futures::stream::once(async move { Ok(bytes) })) } else { Body::from(bytes) }
        });
        Ok(RequestConfig {
            method: self.method.as_ref().map(|v| v.parse()).transpose()?,
            headers,
            body,
            timeout: self.timeout,
            redirect: match self.redirect.as_deref() {
                Some("manual") => Redirect::Manual,
                Some("error") => Redirect::Error,
                _ => Redirect::Follow,
            },
            ..Default::default()
        })
    }
}
pub fn certificate() -> Result<()> {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    println!("{}", json!({"cert":certificate.cert.pem(),"key":certificate.signing_key.serialize_pem()}));
    Ok(())
}
