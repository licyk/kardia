//! `cargo xtask js-tag`: create an annotated tag on `HEAD`, and optionally push
//! it. A `js-v<version>` tag starts the npm release workflow, so it must name
//! the current version of the npm packages. The crates are released per package
//! by `cargo xtask release`, whose `release-<date>` batches carry no version.

use std::path::Path;

use crate::git::git;
use crate::version::Version;

/// What a tag releases: its prefix, its current version and how to name it.
pub struct Release {
    /// `js-v` for the npm packages.
    pub prefix: &'static str,
    /// The release title: `kardia` or `@kardiajs`.
    pub product: &'static str,
    /// What the version is the version of, for errors.
    pub what: &'static str,
    /// The xtask command that bumps the version.
    pub bump: &'static str,
    pub version: Version,
}

#[derive(Debug, Default, clap::Args)]
pub struct Options {
    /// The version the tag names; it must be the release's current version.
    #[arg(long, value_name = "X.Y.Z", conflicts_with = "name")]
    pub version: Option<String>,
    /// Any other tag name.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    #[arg(long, short, value_name = "TEXT")]
    pub message: Option<String>,
    /// Push the tag to the remote.
    #[arg(long)]
    pub push: bool,
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,
    /// Print what would be tagged without creating it.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(root: &Path, options: &Options, release: &Release) -> Result<(), String> {
    let (tag, message) = tag_for(release, options)?;
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

/// The tag and its message. A version tag must be the release's current version.
pub fn tag_for(release: &Release, options: &Options) -> Result<(String, String), String> {
    let prefix = release.prefix;
    let (tag, version) = match (&options.name, &options.version) {
        (Some(name), _) => (name.clone(), None),
        (None, Some(text)) => {
            let version = Version::parse(text.trim_start_matches(prefix).trim_start_matches('v'))?;
            if version != release.version {
                return Err(format!(
                    "{prefix}{version} is not the {} version {}: bump it with `cargo xtask {}` and commit first",
                    release.what, release.version, release.bump
                ));
            }
            (format!("{prefix}{version}"), Some(version))
        }
        (None, None) => (format!("{prefix}{}", release.version), Some(release.version)),
    };
    if tag.is_empty() || tag.contains(char::is_whitespace) {
        return Err(format!("`{tag}` is not a valid tag name"));
    }
    let message = match (&options.message, version) {
        (Some(message), _) => message.clone(),
        (None, Some(version)) => format!("{} {version}", release.product),
        (None, None) => tag.clone(),
    };
    Ok((tag, message))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const PACKAGE: &str = "{\n  \"name\": \"@kardiajs/core\",\n  \"version\": \"0.3.1\"\n}\n";

    fn options(args: &[&str]) -> Options {
        use clap::Parser;
        let mut argv = vec!["cargo xtask".to_string(), "js-tag".to_string()];
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        match crate::cli::Cli::parse_from(argv).task {
            crate::cli::Task::JsTag { version, name, message, push, remote, dry_run } => {
                Options { version, name, message, push, remote, dry_run }
            }
            other => panic!("not a js-tag command: {other:?}"),
        }
    }

    fn release(version: &str) -> Release {
        Release {
            prefix: "js-v",
            product: "@kardiajs",
            what: "npm packages",
            bump: "js-version",
            version: Version::parse(version).unwrap(),
        }
    }

    #[test]
    fn names_the_npm_version_by_default() {
        let (tag, message) = tag_for(&release("0.1.2"), &options(&[])).unwrap();
        assert_eq!((tag.as_str(), message.as_str()), ("js-v0.1.2", "@kardiajs 0.1.2"));
    }

    #[test]
    fn accepts_the_release_version_with_or_without_its_prefix() {
        for given in ["0.2.0", "js-v0.2.0"] {
            assert_eq!(tag_for(&release("0.2.0"), &options(&["--version", given])).unwrap().0, "js-v0.2.0");
        }
    }

    #[test]
    fn refuses_a_version_that_is_not_the_release_version() {
        let error = tag_for(&release("0.2.0"), &options(&["--version", "0.3.0"])).unwrap_err();
        assert!(
            error.contains("not the npm packages version 0.2.0") && error.contains("cargo xtask js-version"),
            "{error}"
        );
    }

    #[test]
    fn takes_any_name_and_message() {
        let (tag, message) = tag_for(&release("0.1.2"), &options(&["--name", "docs-1", "-m", "Docs"])).unwrap();
        assert_eq!((tag.as_str(), message.as_str()), ("docs-1", "Docs"));
        assert!(tag_for(&release("0.1.2"), &options(&["--name", "two words"])).is_err());
        use clap::Parser;
        let refused = crate::cli::Cli::try_parse_from(["cargo xtask", "js-tag", "--version", "1.0.0", "--name", "x"]);
        assert!(refused.is_err(), "--version and --name are mutually exclusive");
    }

    #[test]
    fn tags_and_pushes_in_a_real_repository() {
        let (repo, remote) = repository("push");
        let opts = Options { push: true, ..Options::default() };
        run(&repo, &opts, &release("0.3.1")).unwrap();
        assert_eq!(git(&repo, &["tag", "--list"]).unwrap().trim(), "js-v0.3.1");
        assert_eq!(git(&repo, &["cat-file", "-t", "js-v0.3.1"]).unwrap().trim(), "tag", "annotated");
        assert!(git(&remote, &["tag", "--list"]).unwrap().contains("js-v0.3.1"));

        // the same tag again is refused
        let error = run(&repo, &Options::default(), &release("0.3.1")).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn refuses_a_dirty_tree_and_dry_runs_change_nothing() {
        let (repo, _) = repository("dirty");
        run(&repo, &Options { dry_run: true, ..Options::default() }, &release("0.3.1")).unwrap();
        assert_eq!(git(&repo, &["tag", "--list"]).unwrap().trim(), "");

        std::fs::write(repo.join("package.json"), PACKAGE.replace("0.3.1", "0.3.2")).unwrap();
        let error = run(&repo, &Options::default(), &release("0.3.1")).unwrap_err();
        assert!(error.contains("uncommitted"), "{error}");
    }

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
        std::fs::write(repo.join("package.json"), PACKAGE).unwrap();
        git(&repo, &["add", "."]).unwrap();
        git(&repo, &["commit", "--quiet", "-m", "init"]).unwrap();
        git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
        (repo, remote)
    }
}
