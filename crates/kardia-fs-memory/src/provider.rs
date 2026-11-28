//! The `mem:` provider and the plugin that mounts it.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, stream};
use kardia_core::{Context, Inject, Plugin, Result};
use kardia_fs::{
    Bytes, Capabilities, CopyOptions, DirEntry, EntryType, FileStream, Fs, FsCode, FsError, FsExt, FsProvider,
    MkdirOptions, ReadOptions, Stats, Url, WriteOptions,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::tree::Tree;

/// An in-memory file system. One instance holds one tree, which every location
/// below its mount reads.
pub struct MemoryFs {
    tree: Mutex<Tree>,
}

impl Default for MemoryFs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryFs {
    pub fn new() -> Self {
        Self { tree: Mutex::new(Tree::new()) }
    }

    /// The content of every file, by location, for a caller that inspects the
    /// tree. Directories are reported without content.
    pub fn entries(&self) -> Vec<(String, EntryType, Vec<u8>)> {
        let tree = self.tree.lock();
        let mut entries = Vec::new();
        for (key, kind, data) in tree.dump() {
            entries.push((key, kind, data));
        }
        entries
    }

    /// Creates the directories that hold `url`, so a file can be written without
    /// the caller creating them first.
    async fn mkdir_dirs(&self, url: &Url) -> Result<()> {
        let key = Tree::key(url);
        let Some(parent) = key.rsplit_once('/').map(|(parent, _)| parent) else { return Ok(()) };
        if parent.is_empty() {
            return Ok(());
        }
        // The tree keys by the path of a URL, so a directory is created from the
        // path alone rather than by parsing a new URL.
        self.tree.lock().mkdir_key(parent)
    }
}

impl FsProvider for MemoryFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            write: true,
            rename: true,
            remove: true,
            stat: true,
            mkdir: true,
            copy: true,
            copy_recursive: true,
            realpath: true,
            symlink: true,
            readdir: true,
            links: true,
            read_range: true,
            atomic_write: true,
        }
    }

    fn read(&self, url: Url) -> FileStream {
        self.read_range(url, ReadOptions::default())
    }

    fn read_range(&self, url: Url, options: ReadOptions) -> FileStream {
        let result = (|| {
            let tree = self.tree.lock();
            let kind = tree.kind(&url)?;
            if kind == EntryType::Directory {
                return Err(FsError::new(FsCode::IsADirectory, Some(url), "expected a file").into());
            }
            let node =
                tree.get(&url)?.ok_or_else(|| FsError::new(FsCode::NotFound, Some(url.clone()), "no such file"))?;
            if let Some(expected) = &options.if_match {
                tree.check_version(&url, expected)?;
            }
            let bytes = Bytes::from(node.data.clone());
            let size = bytes.len() as i64;
            Ok::<Bytes, kardia_core::Error>(kardia_fs::slice(&bytes, options.start, options.end, size))
        })();
        match result {
            Ok(bytes) => stream::iter([Ok(bytes)]).boxed(),
            Err(error) => stream::iter([Err(error)]).boxed(),
        }
    }

    fn write(&self, url: Url, data: Vec<u8>) -> BoxFuture<'static, Result<()>> {
        let future = self.write_with(url, data, WriteOptions::default());
        async move {
            future.await?;
            Ok(())
        }
        .boxed()
    }

    fn write_with(&self, url: Url, data: Vec<u8>, options: WriteOptions) -> BoxFuture<'static, Result<Option<String>>> {
        let mut tree = self.tree.lock();
        let result = (|| {
            if let Some(expected) = &options.if_match {
                tree.check_version(&url, expected)?;
            }
            tree.write(&url, data, options.mode, options.exclusive)
        })();
        async move { result.map(Some) }.boxed()
    }

    fn rename(&self, from: Url, to: Url) -> BoxFuture<'static, Result<()>> {
        let result = self.tree.lock().rename(&from, &to);
        async move { result }.boxed()
    }

    fn remove(&self, url: Url) -> BoxFuture<'static, Result<()>> {
        let result = self.tree.lock().remove(&url, false);
        async move { result }.boxed()
    }

    fn remove_with(&self, url: Url, options: kardia_fs::RemoveOptions) -> BoxFuture<'static, Result<()>> {
        let mut tree = self.tree.lock();
        let result = match tree.remove(&url, options.recursive) {
            Err(error)
                if options.force && error.downcast_ref::<FsError>().is_some_and(|e| e.code == FsCode::NotFound) =>
            {
                Ok(())
            }
            result => result,
        };
        async move { result }.boxed()
    }

    fn stat(&self, url: Url) -> BoxFuture<'static, Result<Stats>> {
        let result = (|| {
            let tree = self.tree.lock();
            let kind = tree.kind(&url)?;
            let node = tree.get(&url)?;
            Ok::<Stats, kardia_core::Error>(Stats {
                kind,
                size: node.map(|node| node.data.len() as u64).unwrap_or(0),
                mtime: node.map(|node| node.mtime),
                mode: Some(node.map(|node| node.mode).unwrap_or(0o755)),
                version: node.filter(|node| node.kind == EntryType::File).map(|node| node.version.clone()),
            })
        })();
        async move { result }.boxed()
    }

    fn mkdir(&self, url: Url, options: MkdirOptions) -> BoxFuture<'static, Result<()>> {
        let result = self.tree.lock().mkdir(&url, options.recursive);
        async move { result }.boxed()
    }

    fn copy(&self, from: Url, to: Url, options: CopyOptions) -> BoxFuture<'static, Result<()>> {
        let result = self.tree.lock().copy(&from, &to, options.recursive);
        async move { result }.boxed()
    }

    fn realpath(&self, url: Url) -> BoxFuture<'static, Result<Url>> {
        // The tree has no links below a location, so a resolved location is the
        // location itself once it exists.
        let result = (|| {
            self.tree.lock().kind(&url)?;
            Ok::<Url, kardia_core::Error>(url.clone())
        })();
        async move { result }.boxed()
    }

    fn symlink(&self, target: String, url: Url) -> BoxFuture<'static, Result<()>> {
        let result = self.tree.lock().symlink(&url, target.into_bytes());
        async move { result }.boxed()
    }

    fn readlink(&self, url: Url) -> BoxFuture<'static, Result<String>> {
        let result = (|| {
            let tree = self.tree.lock();
            let node =
                tree.get(&url)?.ok_or_else(|| FsError::new(FsCode::NotFound, Some(url.clone()), "no such entry"))?;
            if node.kind != EntryType::Symlink {
                return Err(FsError::new(FsCode::NotALink, Some(url), "not a symbolic link").into());
            }
            Ok::<String, kardia_core::Error>(String::from_utf8_lossy(&node.data).into_owned())
        })();
        async move { result }.boxed()
    }

    fn readdir(&self, url: Url) -> BoxFuture<'static, Result<Vec<DirEntry>>> {
        let result = self.tree.lock().readdir(&url);
        async move { result }.boxed()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryFsConfig {
    /// The prefix the provider serves; `mem:` by default, and the empty string
    /// serves every location no other mount claims.
    #[serde(default = "mount")]
    pub mount: String,
    /// The files the tree starts with, keyed by their path below the mount.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl Default for MemoryFsConfig {
    fn default() -> Self {
        Self { mount: mount(), files: BTreeMap::new() }
    }
}

fn mount() -> String {
    "mem:".into()
}

pub struct MemoryFsPlugin;
impl Plugin for MemoryFsPlugin {
    const NAME: &'static str = "fs:memory";
    type Config = MemoryFsConfig;

    fn inject() -> Inject {
        Inject::new().with("std:fs")
    }

    async fn apply(ctx: Context, config: Arc<MemoryFsConfig>) -> Result<()> {
        let fs = ctx.get::<Fs>()?;
        let provider = MemoryFs::new();
        // The files are created before the mount, so a provider at the empty
        // string does not route its own writes to another mount.
        // The tree keys an entry by the path of its URL, so the initial files only
        // need a URL of the mounted scheme; it is not resolved against the mount
        // because a mount is a prefix of a location, not a location itself.
        let root = match config.mount.as_str() {
            // The empty prefix serves every location, so the initial files are
            // placed at the base URL the entry resolves against.
            "" => fs.base_url()?,
            mount => match mount.strip_suffix(':') {
                // A placeholder host keeps the path hierarchical; the tree keys by path.
                Some(scheme) if !mount.contains('/') => Url::parse(&format!("{scheme}://local/"))?,
                _ => fs.resolve(mount)?,
            },
        };
        for (path, content) in &config.files {
            let url = fs.resolve_at(&root, path)?;
            // A declared file implies the directories that hold it.
            provider.mkdir_dirs(&url).await?;
            provider.write(url, content.as_bytes().to_vec()).await?;
        }
        fs.mount(&config.mount, Arc::new(provider))?;
        Ok(())
    }
}
