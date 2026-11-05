//! `cargo xtask tag`: creates an annotated tag on `HEAD`, and optionally
//! pushes it. A `v<version>` tag starts the release workflow, so it must name
//! the workspace version.

use std::path::Path;

use crate::git::git;
use crate::version::{Version, workspace_version};

#[derive(Debug, Default)]
pub struct Options {
    pub version: Option<String>,
    pub name: Option<String>,
    pub message: Option<String>,
    pub push: bool,
    pub remote: Option<String>,
    pub dry_run: bool,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| args.next().cloned().ok_or(format!("{flag} needs a value"));
            match arg.as_str() {
                "--version" => options.version = Some(value("--version")?),
                "--name" => options.name = Some(value("--name")?),
                "--message" | "-m" => options.message = Some(value("--message")?),
                "--remote" => options.remote = Some(value("--remote")?),
                "--push" => options.push = true,
                "--dry-run" => options.dry_run = true,
                other => return Err(format!("unexpected argument `{other}`")),
            }
        }
        if options.version.is_some() && options.name.is_some() {
            return Err("give either --version or --name, not both".into());
        }
        Ok(options)
    }
}

pub fn run(root: &Path, options: &Options) -> Result<(), String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).map_err(|error| format!("Cargo.toml: {error}"))?;
    let (tag, message) = tag_for(workspace_version(&manifest)?, options)?;
    let remote = options.remote.as_deref().unwrap_or("origin");

    if !git(root, &["status", "--porcelain"])?.trim().is_empty() {
        return Err("the working tree has uncommitted changes: commit them first, so the tag names them".into());
    }
    if git(root, &["tag", "--list", &tag])?.trim() == tag {
        return Err(format!("tag {tag} already exists"));
    }
    if options.push && !git(root, &["ls-remote", "--tags", remote, &format!("refs/tags/{tag}")])?.trim().is_empty() {
        return Err(format!("tag {tag} already exists on {remote}"));
    }

    let head = git(root, &["rev-parse", "--short", "HEAD"])?;
    let pushing = if options.push { format!(", then push it to {remote}") } else { String::new() };
    if options.dry_run {
        println!("Would tag {} as {tag} (\"{message}\"){pushing}", head.trim());
        return Ok(());
    }
    git(root, &["tag", "--annotate", &tag, "--message", &message])?;
    println!("Tagged {} as {tag}", head.trim());
    if options.push {
        git(root, &["push", remote, &format!("refs/tags/{tag}")])?;
        println!("Pushed {tag} to {remote}");
    }
    Ok(())
}

/// The tag and its message. A version tag must be the workspace version.
pub fn tag_for(workspace: Version, options: &Options) -> Result<(String, String), String> {
    let (tag, version) = match (&options.name, &options.version) {
        (Some(name), _) => (name.clone(), None),
        (None, Some(text)) => {
            let version = Version::parse(text.trim_start_matches('v'))?;
            if version != workspace {
                return Err(format!(
                    "v{version} is not the workspace version {workspace}: \
                     bump it with `cargo xtask version` and commit first"
                ));
            }
            (format!("v{version}"), Some(version))
        }
        (None, None) => (format!("v{workspace}"), Some(workspace)),
    };
    if tag.is_empty() || tag.contains(char::is_whitespace) {
        return Err(format!("`{tag}` is not a valid tag name"));
    }
    let message = match (&options.message, version) {
        (Some(message), _) => message.clone(),
        (None, Some(version)) => format!("kardia {version}"),
        (None, None) => tag.clone(),
    };
    Ok((tag, message))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn options(args: &[&str]) -> Options {
        Options::parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn names_the_workspace_version_by_default() {
        let (tag, message) = tag_for(version("0.1.2"), &options(&[])).unwrap();
        assert_eq!((tag.as_str(), message.as_str()), ("v0.1.2", "kardia 0.1.2"));
    }

    #[test]
    fn accepts_the_workspace_version_with_or_without_v() {
        for given in ["0.1.2", "v0.1.2"] {
            let (tag, _) = tag_for(version("0.1.2"), &options(&["--version", given])).unwrap();
            assert_eq!(tag, "v0.1.2");
        }
    }

    #[test]
    fn refuses_a_version_that_is_not_the_workspace_version() {
        let error = tag_for(version("0.1.2"), &options(&["--version", "0.1.3"])).unwrap_err();
        assert!(error.contains("not the workspace version 0.1.2"), "{error}");
    }

    #[test]
    fn takes_any_name_and_message() {
        let (tag, message) = tag_for(version("0.1.2"), &options(&["--name", "docs-1", "-m", "Docs"])).unwrap();
        assert_eq!((tag.as_str(), message.as_str()), ("docs-1", "Docs"));
        assert!(tag_for(version("0.1.2"), &options(&["--name", "two words"])).is_err());
        assert!(Options::parse(&["--version".into(), "1.0.0".into(), "--name".into(), "x".into()]).is_err());
    }

    #[test]
    fn tags_and_pushes_in_a_real_repository() {
        let (repo, remote) = repository("push");
        let opts = Options { push: true, ..Options::default() };
        run(&repo, &opts).unwrap();
        assert_eq!(git(&repo, &["tag", "--list"]).unwrap().trim(), "v0.3.1");
        assert_eq!(git(&repo, &["cat-file", "-t", "v0.3.1"]).unwrap().trim(), "tag", "annotated");
        assert!(git(&remote, &["tag", "--list"]).unwrap().contains("v0.3.1"));

        // the same tag again is refused
        let error = run(&repo, &Options::default()).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn refuses_a_dirty_tree_and_dry_runs_change_nothing() {
        let (repo, _) = repository("dirty");
        run(&repo, &Options { dry_run: true, ..Options::default() }).unwrap();
        assert_eq!(git(&repo, &["tag", "--list"]).unwrap().trim(), "");

        std::fs::write(repo.join("Cargo.toml"), MANIFEST.replace("0.3.1", "0.3.2")).unwrap();
        let error = run(&repo, &Options::default()).unwrap_err();
        assert!(error.contains("uncommitted"), "{error}");
    }

    const MANIFEST: &str = "[workspace]\n\n[workspace.package]\nversion = \"0.3.1\"\n";

    /// A repository with one commit, and a bare remote named origin.
    fn repository(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("xtask-tag-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (repo, remote) = (base.join("repo"), base.join("remote.git"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--quiet", "--bare"]).unwrap();
        git(&repo, &["init", "--quiet"]).unwrap();
        for (key, value) in [
            ("user.name", "xtask"),
            ("user.email", "xtask@example.com"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ] {
            git(&repo, &["config", key, value]).unwrap();
        }
        std::fs::write(repo.join("Cargo.toml"), MANIFEST).unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "--quiet", "-m", "init"]).unwrap();
        git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
        (repo, remote)
    }
}
