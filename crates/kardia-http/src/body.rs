use bytes::{Bytes, BytesMut};
use futures::{StreamExt, stream::BoxStream};
use kardia_core::{EffectHandle, Result};

pub type ByteStream = BoxStream<'static, Result<Bytes>>;
pub enum Body {
    Bytes(Bytes),
    Stream(ByteStream),
}
impl Body {
    pub fn stream(source: impl futures::Stream<Item = Result<Bytes>> + Send + 'static) -> Self {
        Self::Stream(source.boxed())
    }
    pub fn into_stream(self) -> ByteStream {
        match self {
            Self::Bytes(bytes) => futures::stream::once(async move { Ok(bytes) }).boxed(),
            Self::Stream(stream) => stream,
        }
    }
    pub async fn bytes(self) -> Result<Bytes> {
        let mut source = self.into_stream();
        let mut out = BytesMut::new();
        while let Some(bytes) = source.next().await {
            out.extend(bytes?);
        }
        Ok(out.freeze())
    }
    pub(crate) fn replay(&self) -> Option<Self> {
        match self {
            Self::Bytes(bytes) => Some(Self::Bytes(bytes.clone())),
            Self::Stream(_) => None,
        }
    }
}
impl From<Bytes> for Body {
    fn from(b: Bytes) -> Self {
        Self::Bytes(b)
    }
}
impl From<Vec<u8>> for Body {
    fn from(b: Vec<u8>) -> Self {
        Self::Bytes(b.into())
    }
}
impl From<String> for Body {
    fn from(b: String) -> Self {
        Self::Bytes(b.into())
    }
}
impl From<&'static str> for Body {
    fn from(b: &'static str) -> Self {
        Self::Bytes(Bytes::from_static(b.as_bytes()))
    }
}
pub(crate) struct Operation(pub EffectHandle);
impl Drop for Operation {
    fn drop(&mut self) {
        drop(self.0.dispose());
    }
}
