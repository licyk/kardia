use crate::HttpConfig;
use kardia_core::{Context, Disposer, Plugin, Result, Service};
use std::sync::Arc;
use tokio::sync::watch;

pub struct Http {
    pub config: HttpConfig,
    pub(crate) client: reqwest::Client,
    pub(crate) stop: watch::Sender<bool>,
    pub(crate) active: watch::Sender<usize>,
}
impl Service for Http {
    const NAME: &'static str = "std:http";
}
impl Http {
    pub fn active_operations(&self) -> usize {
        *self.active.borrow()
    }
    /// The client for a request: the cached one when its proxy applies, and a
    /// direct one when the target bypasses `noProxy`.
    pub(crate) fn client(&self, proxy: Option<&str>, url: &url::Url, no_proxy: &[String]) -> Result<reqwest::Client> {
        if crate::config::bypass_proxy(url, no_proxy) {
            return build(&self.config, None);
        }
        if proxy == self.config.proxy_agent.as_deref() { Ok(self.client.clone()) } else { build(&self.config, proxy) }
    }
}
pub struct HttpPlugin;
impl Plugin for HttpPlugin {
    const NAME: &'static str = "http";
    type Config = HttpConfig;
    fn provide() -> Vec<kardia_core::Key> {
        vec!["http".into()]
    }
    async fn apply(ctx: Context, config: Arc<HttpConfig>) -> Result<()> {
        let client = build(&config, config.proxy_agent.as_deref())
            .map_err(|error| kardia_core::ValidationError::message(error.to_string()))?;
        let (stop, _) = watch::channel(false);
        let (active, _) = watch::channel(0usize);
        let service = Http { config: (*config).clone(), client, stop: stop.clone(), active: active.clone() };
        ctx.provide(service)?;
        ctx.effect_sync("http client", |scope| {
            scope.defer(Disposer::future(move || {
                stop.send_replace(true);
                let mut active = active.subscribe();
                async move {
                    let _ = active.wait_for(|count| *count == 0).await;
                }
            }));
            Ok(())
        })?;
        Ok(())
    }
}
fn build(config: &HttpConfig, proxy: Option<&str>) -> Result<reqwest::Client> {
    use rustls::pki_types::pem::PemObject;
    let certs = config
        .root_certificates
        .iter()
        .map(|pem| rustls::pki_types::CertificateDer::from_pem_slice(pem.as_bytes()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(certs, provider.clone())?;
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let mut builder =
        reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).tls_backend_preconfigured(tls);
    if let Some(proxy) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}
pub(crate) struct Active(pub watch::Sender<usize>);
impl Active {
    pub fn new(counter: &watch::Sender<usize>) -> Self {
        counter.send_modify(|n| *n += 1);
        Self(counter.clone())
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}
