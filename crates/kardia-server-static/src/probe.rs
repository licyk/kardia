use bytes::{Buf, BytesMut};
use futures::StreamExt;
use kardia_core::Result;
use kardia_fs::FileStream;

pub(super) struct Probe {
    source: FileStream,
    buffered: BytesMut,
    ended: bool,
    pub position: usize,
}
impl Probe {
    pub fn new(source: FileStream) -> Self {
        Self { source, buffered: BytesMut::new(), ended: false, position: 0 }
    }
    pub async fn peek(&mut self, bytes: &mut [u8]) -> Result<usize> {
        while self.buffered.len() < bytes.len() && !self.ended {
            match self.source.next().await {
                Some(chunk) => self.buffered.extend_from_slice(&chunk?),
                None => self.ended = true,
            }
        }
        let length = bytes.len().min(self.buffered.len());
        bytes[..length].copy_from_slice(&self.buffered[..length]);
        Ok(length)
    }
    pub async fn read(&mut self, length: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![0; length];
        if self.peek(&mut bytes).await? != length {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        self.buffered.advance(length);
        self.position += length;
        Ok(bytes)
    }
    pub async fn skip(&mut self, mut length: usize) -> Result<()> {
        while length > 0 {
            let count = length.min(65536);
            self.read(count).await?;
            length -= count;
        }
        Ok(())
    }
}
