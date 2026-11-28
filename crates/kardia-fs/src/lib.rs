//! File-system service (`"fs"`) with URL-based read providers and native watches.
//! Mounts, streams and watches belong to their caller's lifecycle.

mod access;
mod dir;
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
pub use dir::{DirEntryRef, DirRef};
pub use error::{FsCode, FsError};
pub use faulty::{Fault, Faulty};
pub use file::{
    Capabilities, CopyOptions, DirEntry, EntryType, FileRef, FileStream, FsProvider, MkdirOptions, ReadOptions,
    RemoveOptions, Stats, WriteOptions, slice,
};
pub use filter::{Filter, Predicate, admits, compile};
pub use local::{LocalFs, LocalFsConfig, LocalFsPlugin};
pub use service::{Fs, FsConfig, FsPlugin, Provider, WatchOptions};
pub use watcher::{Subscription, Watcher};
