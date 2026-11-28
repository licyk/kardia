//! The entries of one mount, keyed by their path below it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use kardia_fs::{EntryType, FsCode, FsError};
use url::Url;

pub(crate) struct Node {
    pub(crate) kind: EntryType,
    /// The content of a file, or the target of a symbolic link.
    pub(crate) data: Vec<u8>,
    /// The version of the content, which a write changes.
    pub(crate) version: String,
    pub(crate) mode: u32,
    pub(crate) mtime: SystemTime,
}

#[derive(Default)]
pub(crate) struct Tree {
    nodes: BTreeMap<String, Node>,
    next_version: AtomicU64,
}

impl Tree {
    pub(crate) fn new() -> Self {
        Self { nodes: BTreeMap::new(), next_version: AtomicU64::new(1) }
    }

    fn version(&self) -> String {
        format!("{}", self.next_version.fetch_add(1, Ordering::Relaxed))
    }

    /// The key of a location below the mount, without a trailing separator. The
    /// root is the empty string.
    pub(crate) fn key(url: &Url) -> String {
        url.path().trim_start_matches('/').trim_end_matches('/').to_owned()
    }

    fn parent_of(key: &str) -> Option<&str> {
        if key.is_empty() {
            return None;
        }
        Some(key.rsplit_once('/').map(|(parent, _)| parent).unwrap_or_default())
    }

    /// The node at `key`, or `NotFound`. The root is a directory that is not
    /// stored, so it is reported from the request rather than from a node.
    pub(crate) fn get(&self, url: &Url) -> Result<Option<&Node>, FsError> {
        Ok(self.nodes.get(&Self::key(url)))
    }

    pub(crate) fn kind(&self, url: &Url) -> Result<EntryType, kardia_core::Error> {
        let key = Self::key(url);
        if key.is_empty() {
            return Ok(EntryType::Directory);
        }
        self.nodes
            .get(&key)
            .map(|node| node.kind)
            .ok_or_else(|| FsError::new(FsCode::NotFound, Some(url.clone()), "no such entry").into())
    }

    /// Checks that every parent of `key` exists and is a directory.
    fn require_parent(&self, url: &Url) -> Result<(), kardia_core::Error> {
        let key = Self::key(url);
        let Some(parent) = Self::parent_of(&key) else { return Ok(()) };
        if parent.is_empty() {
            return Ok(());
        }
        match self.nodes.get(parent) {
            Some(node) if node.kind == EntryType::Directory => Ok(()),
            Some(_) => {
                Err(FsError::new(FsCode::NotADirectory, Some(url.clone()), "a parent is not a directory").into())
            }
            None => Err(FsError::new(FsCode::NotFound, Some(url.clone()), "a parent is missing").into()),
        }
    }

    pub(crate) fn write(
        &mut self,
        url: &Url,
        data: Vec<u8>,
        mode: Option<u32>,
        exclusive: bool,
    ) -> Result<String, kardia_core::Error> {
        let key = Self::key(url);
        if key.is_empty() {
            return Err(FsError::new(FsCode::IsADirectory, Some(url.clone()), "the root is a directory").into());
        }
        self.require_parent(url)?;
        if exclusive && self.nodes.contains_key(&key) {
            return Err(FsError::new(FsCode::AlreadyExists, Some(url.clone()), "the file exists").into());
        }
        let version = self.version();
        let mtime = SystemTime::now();
        self.nodes
            .entry(key)
            .and_modify(|node| {
                node.kind = EntryType::File;
                node.data = data.clone();
                node.version = version.clone();
                node.mtime = mtime;
                if let Some(mode) = mode {
                    node.mode = mode;
                }
            })
            .or_insert(Node {
                kind: EntryType::File,
                data,
                version: version.clone(),
                mode: mode.unwrap_or(0o644),
                mtime,
            });
        Ok(version)
    }

    pub(crate) fn check_version(&self, url: &Url, expected: &str) -> Result<(), kardia_core::Error> {
        let Some(node) = self.nodes.get(&Self::key(url)) else {
            return Err(FsError::new(FsCode::VersionMismatch, Some(url.clone()), "no such file").into());
        };
        if node.kind != EntryType::File {
            return Err(FsError::new(FsCode::VersionMismatch, Some(url.clone()), "not a file").into());
        }
        if node.version != expected {
            return Err(FsError::new(FsCode::VersionMismatch, Some(url.clone()), "the file has another version").into());
        }
        Ok(())
    }

    pub(crate) fn mkdir(&mut self, url: &Url, recursive: bool) -> Result<(), kardia_core::Error> {
        let key = Self::key(url);
        if key.is_empty() {
            return if recursive {
                Ok(())
            } else {
                Err(FsError::new(FsCode::AlreadyExists, Some(url.clone()), "the root is a directory").into())
            };
        }
        if let Some(node) = self.nodes.get(&key) {
            return if recursive && node.kind == EntryType::Directory {
                Ok(())
            } else {
                Err(FsError::new(FsCode::AlreadyExists, Some(url.clone()), "the entry exists").into())
            };
        }
        if recursive {
            // Every missing ancestor becomes a directory.
            let mut current = String::new();
            for name in key.split('/') {
                if !current.is_empty() {
                    current.push('/');
                }
                current.push_str(name);
                let version = self.version();
                let node = self.nodes.entry(current.clone()).or_insert_with(|| Node::directory(&version));
                if node.kind != EntryType::Directory {
                    return Err(
                        FsError::new(FsCode::NotADirectory, Some(url.clone()), "a parent is not a directory").into()
                    );
                }
            }
        } else {
            self.require_parent(url)?;
            self.nodes.insert(key, Node::directory(&self.version()));
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, url: &Url, recursive: bool) -> Result<(), kardia_core::Error> {
        let key = Self::key(url);
        let Some(node) = self.nodes.get(&key) else {
            return Err(FsError::new(FsCode::NotFound, Some(url.clone()), "no such entry").into());
        };
        if node.kind == EntryType::Directory {
            let prefix = format!("{key}/");
            let children: Vec<String> = self
                .nodes
                .range(prefix.clone()..)
                .take_while(|(k, _)| k.starts_with(&prefix))
                .map(|(k, _)| k.clone())
                .collect();
            if !children.is_empty() && !recursive {
                return Err(
                    FsError::new(FsCode::DirectoryNotEmpty, Some(url.clone()), "the directory is not empty").into()
                );
            }
            for child in children {
                self.nodes.remove(&child);
            }
        }
        self.nodes.remove(&key);
        Ok(())
    }

    pub(crate) fn rename(&mut self, from: &Url, to: &Url) -> Result<(), kardia_core::Error> {
        let (from_key, to_key) = (Self::key(from), Self::key(to));
        if !self.nodes.contains_key(&from_key) {
            return Err(FsError::new(FsCode::NotFound, Some(from.clone()), "no such entry").into());
        }
        self.require_parent(to)?;
        // A directory moves with every entry below it.
        let moved: Vec<(String, Node)> = self
            .nodes
            .range(from_key.clone()..)
            .take_while(|(key, _)| **key == from_key || key.starts_with(&format!("{from_key}/")))
            .map(|(key, node)| {
                (
                    key.clone(),
                    Node {
                        kind: node.kind,
                        data: node.data.clone(),
                        version: node.version.clone(),
                        mode: node.mode,
                        mtime: node.mtime,
                    },
                )
            })
            .collect();
        for (key, _) in &moved {
            self.nodes.remove(key);
        }
        for (key, mut node) in moved {
            let target = if key == from_key { to_key.clone() } else { format!("{to_key}{}", &key[from_key.len()..]) };
            node.version = self.version();
            self.nodes.insert(target, node);
        }
        Ok(())
    }

    pub(crate) fn copy(&mut self, from: &Url, to: &Url, recursive: bool) -> Result<(), kardia_core::Error> {
        let (from_key, to_key) = (Self::key(from), Self::key(to));
        let Some(source) = self.nodes.get(&from_key) else {
            return Err(FsError::new(FsCode::NotFound, Some(from.clone()), "no such entry").into());
        };
        if source.kind == EntryType::Directory && !recursive {
            return Err(FsError::new(FsCode::IsADirectory, Some(from.clone()), "expected a file").into());
        }
        self.require_parent(to)?;
        let copied: Vec<(String, Node)> = self
            .nodes
            .range(from_key.clone()..)
            .take_while(|(key, _)| **key == from_key || key.starts_with(&format!("{from_key}/")))
            .map(|(key, node)| {
                (
                    key.clone(),
                    Node {
                        kind: node.kind,
                        data: node.data.clone(),
                        version: self.version(),
                        mode: node.mode,
                        mtime: node.mtime,
                    },
                )
            })
            .collect();
        for (key, node) in copied {
            let target = if key == from_key { to_key.clone() } else { format!("{to_key}{}", &key[from_key.len()..]) };
            self.nodes.insert(target, node);
        }
        Ok(())
    }

    /// Every entry, by path, with its type and content.
    pub(crate) fn dump(&self) -> Vec<(String, EntryType, Vec<u8>)> {
        self.nodes.iter().map(|(key, node)| (key.clone(), node.kind, node.data.clone())).collect()
    }

    pub(crate) fn symlink(&mut self, url: &Url, target: Vec<u8>) -> Result<(), kardia_core::Error> {
        let key = Self::key(url);
        if self.nodes.contains_key(&key) {
            return Err(FsError::new(FsCode::AlreadyExists, Some(url.clone()), "the entry exists").into());
        }
        self.require_parent(url)?;
        let version = self.version();
        self.nodes.insert(
            key,
            Node { kind: EntryType::Symlink, data: target, version, mode: 0o777, mtime: SystemTime::now() },
        );
        Ok(())
    }

    /// Creates `key` and its missing ancestors as directories.
    pub(crate) fn mkdir_key(&mut self, key: &str) -> Result<(), kardia_core::Error> {
        let mut current = String::new();
        for name in key.split('/') {
            if name.is_empty() {
                continue;
            }
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(name);
            let version = self.version();
            let node = self.nodes.entry(current.clone()).or_insert_with(|| Node::directory(&version));
            if node.kind != EntryType::Directory {
                return Err(FsError::new(FsCode::NotADirectory, None, "a parent is not a directory").into());
            }
        }
        Ok(())
    }

    pub(crate) fn readdir(&self, url: &Url) -> Result<Vec<kardia_fs::DirEntry>, kardia_core::Error> {
        let key = Self::key(url);
        let kind = self.kind(url)?;
        if kind != EntryType::Directory {
            return Err(FsError::new(FsCode::NotADirectory, Some(url.clone()), "expected a directory").into());
        }
        let prefix = if key.is_empty() { String::new() } else { format!("{key}/") };
        let mut entries = Vec::new();
        for (child, node) in self.nodes.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix)) {
            let name = &child[prefix.len()..];
            if name.contains('/') {
                continue;
            }
            entries.push(kardia_fs::DirEntry { name: name.to_owned(), kind: node.kind });
        }
        Ok(entries)
    }
}

impl Node {
    fn directory(version: &str) -> Self {
        Self {
            kind: EntryType::Directory,
            data: Vec::new(),
            version: version.to_owned(),
            mode: 0o755,
            mtime: SystemTime::now(),
        }
    }
}
