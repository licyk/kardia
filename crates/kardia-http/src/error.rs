#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("TIMEOUT: request timeout")]
    Timeout,
}
