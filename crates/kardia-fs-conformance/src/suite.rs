use std::sync::Arc;

use crate::case::{Case, Outcome, check, is_code};

use kardia_core::{Context, Result};
use kardia_fs::{
    CopyOptions, DirEntry, EntryType, Fs, FsCode, FsConfig, FsExt, FsPlugin, FsProvider, MkdirOptions, RemoveOptions,
    WriteOptions,
};

/// A provider mounted in a `Fs`, ready to be checked.
pub struct Suite {
    root: Context,
}

impl Suite {
    /// Starts a `Fs` whose only provider is `provider`, mounted at `mem:`.
    pub async fn boot(provider: Arc<dyn FsProvider>) -> Result<Self> {
        Self::boot_at(provider, "mem:", "mem://local/").await
    }

    /// Starts a `Fs` whose provider is mounted at `mount`, with a base URL below
    /// it, so a provider that serves one scheme can be checked too.
    pub async fn boot_at(provider: Arc<dyn FsProvider>, mount: &str, base: &str) -> Result<Self> {
        let root = Context::new();
        root.plugin::<FsPlugin>(FsConfig { base_url: Some(kardia_fs::Url::parse(base)?) })?.settled().await?;
        root.get::<Fs>()?.mount(mount, provider)?;
        Ok(Self { root })
    }

    /// Runs every case, returning each name with its outcome.
    pub async fn run(&self) -> Vec<(&'static str, Outcome)> {
        let cases: [(&'static str, _); 11] = [
            ("write then read returns the content", Case::WriteThenRead),
            ("a missing file reads as NotFound", Case::MissingIsNotFound),
            ("exclusive create rejects an existing file", Case::ExclusiveRejects),
            ("a conditional write rejects a stale version", Case::ConditionalWrite),
            ("a ranged read selects the requested bytes", Case::RangedRead),
            ("stat reports the type and size", Case::StatReports),
            ("mkdir recursive creates the parents", Case::MkdirRecursive),
            ("readdir lists the entries with their types", Case::ReaddirLists),
            ("rename moves an entry", Case::RenameMoves),
            ("remove refuses a non-empty directory", Case::RemoveRefuses),
            ("copy duplicates a file", Case::CopyFile),
        ];
        let mut results = Vec::with_capacity(cases.len());
        for (name, case) in cases {
            // Every case starts from the same empty tree.
            self.reset().await;
            let outcome = match self.check(case).await {
                Ok(()) => Outcome::Passed,
                Err(error) if is_code(&error, FsCode::NotSupported) => Outcome::Skipped,
                Err(error) => Outcome::Failed(error.to_string()),
            };
            results.push((name, outcome));
        }
        results
    }

    /// The failures, as `case: reason`.
    pub async fn failures(&self) -> Vec<String> {
        self.run()
            .await
            .into_iter()
            .filter_map(|(name, outcome)| match outcome {
                Outcome::Failed(reason) => Some(format!("{name}: {reason}")),
                _ => None,
            })
            .collect()
    }

    fn fs(&self) -> kardia_core::Svc<Fs> {
        self.root.get::<Fs>().expect("the suite mounts a provider")
    }

    async fn reset(&self) {
        let fs = self.fs();
        // The tree of the previous case is removed with every entry below it.
        let _ = fs.remove("suite", RemoveOptions { recursive: true, force: true }).await;
        let _ = fs.mkdir("suite", MkdirOptions { recursive: true }).await;
    }

    async fn check(&self, case: Case) -> Result<()> {
        let fs = self.fs();
        match case {
            Case::WriteThenRead => {
                fs.file("suite/a.txt")?.write("hello", WriteOptions::default()).await?;
                check(fs.file("suite/a.txt")?.text().await? == "hello", "content differs")
            }
            Case::MissingIsNotFound => {
                let error = fs.file("suite/missing.txt")?.text().await.unwrap_err();
                check(is_code(&error, FsCode::NotFound), format!("expected NotFound, got {error}"))
            }
            Case::ExclusiveRejects => {
                fs.file("suite/b.txt")?.write("one", WriteOptions::default()).await?;
                let error = fs
                    .file("suite/b.txt")?
                    .write("two", WriteOptions { exclusive: true, ..Default::default() })
                    .await
                    .unwrap_err();
                check(is_code(&error, FsCode::AlreadyExists), format!("expected AlreadyExists, got {error}"))
            }
            Case::ConditionalWrite => {
                fs.file("suite/c.txt")?.write("one", WriteOptions::default()).await?;
                let stale = fs.stat("suite/c.txt").await?.version;
                fs.file("suite/c.txt")?.write("two", WriteOptions::default()).await?;
                let Some(stale) = stale else { return Ok(()) };
                let error = fs
                    .file("suite/c.txt")?
                    .write("three", WriteOptions { if_match: Some(stale), ..Default::default() })
                    .await
                    .unwrap_err();
                check(is_code(&error, FsCode::VersionMismatch), format!("expected VersionMismatch, got {error}"))
            }
            Case::RangedRead => {
                fs.file("suite/d.txt")?.write("0123456789", WriteOptions::default()).await?;
                let file = fs.file("suite/d.txt")?;
                check(file.slice(2, Some(5)).text().await? == "234", "a bounded range differs")?;
                check(file.slice(-3, None).text().await? == "789", "a negative range differs")
            }
            Case::StatReports => {
                fs.file("suite/e.txt")?.write("1234", WriteOptions::default()).await?;
                let stats = fs.stat("suite/e.txt").await?;
                check(stats.kind == EntryType::File, "the type is not a file")?;
                check(stats.size == 4, format!("the size is {}, not 4", stats.size))
            }
            Case::MkdirRecursive => {
                fs.mkdir("suite/x/y/z", MkdirOptions { recursive: true }).await?;
                check(fs.stat("suite/x/y/z").await?.kind == EntryType::Directory, "the directory is missing")
            }
            Case::ReaddirLists => {
                fs.file("suite/f.txt")?.write("x", WriteOptions::default()).await?;
                let mut entries: Vec<DirEntry> = fs.readdir("suite").await?;
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                check(entries.len() == 1, format!("the suite root has {} entries, not 1", entries.len()))?;
                check(entries[0].kind == EntryType::File, "the listed entry is not a file")
            }
            Case::RenameMoves => {
                fs.file("suite/g.txt")?.write("moved", WriteOptions::default()).await?;
                fs.rename("suite/g.txt", "suite/h.txt").await?;
                check(fs.file("suite/h.txt")?.text().await? == "moved", "the content did not follow the rename")
            }
            Case::RemoveRefuses => {
                fs.mkdir("suite/keep", MkdirOptions::default()).await?;
                fs.file("suite/keep/i.txt")?.write("x", WriteOptions::default()).await?;
                let error = fs.remove("suite/keep", RemoveOptions::default()).await.unwrap_err();
                check(is_code(&error, FsCode::DirectoryNotEmpty), format!("expected DirectoryNotEmpty, got {error}"))
            }
            Case::CopyFile => {
                fs.file("suite/j.txt")?.write("copied", WriteOptions::default()).await?;
                fs.copy("suite/j.txt", "suite/k.txt", CopyOptions::default()).await?;
                check(fs.file("suite/k.txt")?.text().await? == "copied", "the copy differs")
            }
        }
    }
}
