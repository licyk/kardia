use crate::ByteStream;
use async_compression::tokio::bufread::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder};
use futures::TryStreamExt;
use kardia_core::Result;
use std::pin::Pin;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio_util::io::{ReaderStream, StreamReader};

type Reader = Pin<Box<dyn AsyncBufRead + Send>>;
pub(crate) async fn decode(response: reqwest::Response, method: &http::Method) -> Result<ByteStream> {
    let codings = response
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let skip = method == http::Method::HEAD || matches!(response.status().as_u16(), 101 | 204 | 205 | 304);
    let source = response.bytes_stream().map_err(std::io::Error::other);
    let mut reader: Reader = Box::pin(BufReader::new(StreamReader::new(source)));
    let codings: Vec<_> = codings.split(',').map(str::trim).collect();
    if !skip && codings.iter().all(|v| matches!(*v, "gzip" | "x-gzip" | "deflate" | "br" | "zstd")) {
        for coding in codings.into_iter().rev() {
            reader = match coding {
                "gzip" | "x-gzip" => {
                    let mut decoder = GzipDecoder::new(reader);
                    decoder.multiple_members(true);
                    Box::pin(BufReader::new(decoder))
                }
                "deflate" => {
                    let first = reader.as_mut().fill_buf().await?.first().copied();
                    if first.is_some_and(|b| b & 15 == 8) {
                        Box::pin(BufReader::new(ZlibDecoder::new(reader)))
                    } else {
                        Box::pin(BufReader::new(DeflateDecoder::new(reader)))
                    }
                }
                "br" => Box::pin(BufReader::new(BrotliDecoder::new(reader))),
                "zstd" => Box::pin(BufReader::new(ZstdDecoder::new(reader))),
                _ => unreachable!(),
            };
        }
    }
    Ok(futures::StreamExt::boxed(ReaderStream::new(reader).map_err(Into::into)))
}
