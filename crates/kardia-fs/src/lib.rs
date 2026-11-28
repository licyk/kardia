//! File-system service (`"fs"`) with URL-based read providers and native watches.
//! Mounts, streams and watches belong to their caller's lifecycle.

mod access;
mod error;
mod faulty;
mod file;
mod filter;
mod local;
mod service;
mod url;
mod watcher;

pub use ::url::Url;
pub use access::FsExt;
pub use bytes::Bytes;
pub use error::{FsCode, FsError};
pub use faulty::{Fault, Faulty};
pub use file::{Capabilities, EntryType, FileRef, FileStream, FsProvider, Stats};
pub use filter::{Filter, Predicate, admits, compile};
pub use local::{LocalFs, LocalFsConfig, LocalFsPlugin};
pub use service::{Fs, FsConfig, FsPlugin, Provider, WatchOptions};
pub use watcher::{Subscription, Watcher};
