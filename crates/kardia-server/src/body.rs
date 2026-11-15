use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::{Bytes, BytesMut};
use futures::{
    StreamExt,
    stream::{self, BoxStream},
};
use kardia_core::Result;
use tokio::sync::Mutex;

pub type ByteStream = BoxStream<'static, Result<Bytes>>;

/// A pull-driven body. Forks retain unread chunks only for live readers.
pub struct Body(ByteStream);

impl Default for Body {
    fn default() -> Self {
        Self(stream::empty().boxed())
    }
}

impl Body {
    pub fn stream(stream: impl futures::Stream<Item = Result<Bytes>> + Send + 'static) -> Self {
        Self(stream.boxed())
    }

    pub fn into_stream(self) -> ByteStream {
        self.0
    }

    pub async fn bytes(self) -> Result<Bytes> {
        let mut source = self.0;
        let mut bytes = BytesMut::new();
        while let Some(chunk) = source.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        Ok(bytes.freeze())
    }

    /// Splits this reader without eagerly draining its source.
    pub fn fork(&mut self) -> Self {
        let source = std::mem::take(self).0;
        let state = Arc::new(Mutex::new(Tee { source, queues: Default::default(), ended: false }));
        let alive = Arc::new([AtomicBool::new(true), AtomicBool::new(true)]);
        let reader = |index| {
            Self::stream(stream::unfold(
                Reader { index, state: state.clone(), alive: alive.clone() },
                |reader| async move {
                    let item = {
                        let mut state = reader.state.lock().await;
                        if let Some(item) = state.queues[reader.index].pop_front() {
                            Some(item)
                        } else if state.ended {
                            None
                        } else {
                            let item = state.source.next().await;
                            if let Some(item) = &item {
                                if reader.alive[1 - reader.index].load(Ordering::Acquire) {
                                    state.queues[1 - reader.index].push_back(item.clone());
                                } else {
                                    state.queues[1 - reader.index].clear();
                                }
                            } else {
                                state.ended = true;
                            }
                            item
                        }
                    };
                    item.map(|item| (item, reader))
                },
            ))
        };
        *self = reader(0);
        reader(1)
    }
}

impl From<Bytes> for Body {
    fn from(bytes: Bytes) -> Self {
        Self::stream(stream::once(async { Ok(bytes) }))
    }
}
impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Bytes::from(bytes).into()
    }
}
impl From<String> for Body {
    fn from(text: String) -> Self {
        Bytes::from(text).into()
    }
}
impl From<&'static str> for Body {
    fn from(text: &'static str) -> Self {
        Bytes::from_static(text.as_bytes()).into()
    }
}

struct Tee {
    source: ByteStream,
    queues: [VecDeque<Result<Bytes>>; 2],
    ended: bool,
}
struct Reader {
    index: usize,
    state: Arc<Mutex<Tee>>,
    alive: Arc<[AtomicBool; 2]>,
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.alive[self.index].store(false, Ordering::Release);
    }
}
