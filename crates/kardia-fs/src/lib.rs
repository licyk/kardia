//! File-system watch service (`"fs"`). A watch is an effect of the caller's
//! fiber and ends with it.

mod filter;
mod service;
mod watcher;

pub use filter::{Filter, Predicate, admits, compile};
pub use service::{Fs, FsExt, FsPlugin, WatchOptions};
pub use watcher::{Subscription, Watcher};
