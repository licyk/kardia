//! File-system service (`"fs"`) with URL-based read providers and native watches.
//! Mounts, streams and watches belong to their caller's lifecycle.

mod access;
mod error;
mod file;
mod filter;
mod local;
mod service;
mod watcher;

pub use access::FsExt;
pub use bytes::Bytes;
pub use error::{FsCode, FsError};
pub use file::{FileRef, FileStream, FsProvider};
pub use filter::{Filter, Predicate, admits, compile};
pub use local::{LocalFs, LocalFsConfig, LocalFsPlugin};
pub use service::{Fs, FsConfig, FsPlugin, WatchOptions};
pub use url::Url;
pub use watcher::{Subscription, Watcher};
