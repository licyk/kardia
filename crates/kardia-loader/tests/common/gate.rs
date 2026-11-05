//! A one-shot gate.

/// A one-shot gate.
#[derive(Clone)]
pub struct Gate(tokio::sync::watch::Sender<bool>);

impl Gate {
    pub fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }

    pub fn open(&self) {
        self.0.send_replace(true);
    }

    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|open| *open).await;
    }
}
