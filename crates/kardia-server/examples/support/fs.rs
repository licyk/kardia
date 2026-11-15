use kardia_core::{Context, Result};
use kardia_fs::{Fs, FsExt, FsPlugin};
use kardia_server::{Server, ServerExt};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    config: Value,
    #[serde(default)]
    intercepts: Vec<String>,
    path: String,
}

pub async fn install(root: &Context) -> Result<()> {
    root.get::<Server>()?.post("/resolve", |req, res, _| async move {
        let input: Input = serde_json::from_value(req.json().await?)?;
        let ctx = Context::new();
        let result: Result<Value> = async {
            ctx.plugin_value::<FsPlugin>(input.config)?.settled().await?;
            let mut scoped = ctx.clone();
            for base in input.intercepts {
                scoped = scoped.intercept("fs", json!({"baseUrl":base}));
            }
            let fs = scoped.get::<Fs>()?;
            Ok(json!({"base":fs.base_url()?.as_str(), "url":fs.resolve(&input.path)?.as_str()}))
        }
        .await;
        ctx.fiber().dispose().await?;
        res.set_json(&result.unwrap_or_else(|_| json!({"error":true})))?;
        Ok(None)
    })?;
    Ok(())
}
