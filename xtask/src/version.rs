//! `cargo xtask version <patch|minor|major>`: bumps the workspace version in
//! every file that names it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use toml_edit::{DocumentMut, value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Patch,
    Minor,
    Major,
}

#[derive(Debug)]
pub struct Options {
    pub level: Level,
    pub dry_run: bool,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut level = None;
        let mut dry_run = false;
        for arg in args {
            match arg.as_str() {
                "patch" => level = Some(Level::Patch),
                "minor" => level = Some(Level::Minor),
                "major" => level = Some(Level::Major),
                "--dry-run" => dry_run = true,
                other => return Err(format!("unexpected argument `{other}`: version <patch|minor|major> [--dry-run]")),
            }
        }
        let level = level.ok_or("which increment: version <patch|minor|major>")?;
        Ok(Self { level, dry_run })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// A plain `major.minor.patch`; pre-releases and build metadata are refused.
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Vec<u64> = text.split('.').map(|part| part.parse().ok()).collect::<Option<_>>().unwrap_or_default();
        match parts[..] {
            [major, minor, patch] => Ok(Self { major, minor, patch }),
            _ => Err(format!("`{text}` is not a plain major.minor.patch version")),
        }
    }

    pub fn bump(self, level: Level) -> Self {
        match level {
            Level::Patch => Self { patch: self.patch + 1, ..self },
            Level::Minor => Self { minor: self.minor + 1, patch: 0, ..self },
            Level::Major => Self { major: self.major + 1, minor: 0, patch: 0 },
        }
    }

    /// The requirement a user writes for this version (`kardia = "0.1"`):
    /// before 1.0 a minor release may break, after it only a major one.
    pub fn requirement(self) -> String {
        if self.major == 0 { format!("0.{}", self.minor) } else { self.major.to_string() }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A file and its new contents.
#[derive(Debug)]
pub struct Change {
    pub path: PathBuf,
    pub contents: String,
}

pub fn run(root: &Path, options: &Options) -> Result<(), String> {
    let (old, new, changes) = plan(root, options.level)?;
    println!("{old} -> {new}");
    for change in &changes {
        println!("  {}", change.path.strip_prefix(root).unwrap_or(&change.path).display());
    }
    if options.dry_run {
        println!("  Cargo.lock (cargo update --workspace)");
        return Ok(());
    }
    for change in &changes {
        std::fs::write(&change.path, &change.contents)
            .map_err(|error| format!("{}: {error}", change.path.display()))?;
    }
    update_lockfile(root)?;
    println!("  Cargo.lock");
    Ok(())
}

/// The workspace version, its bump, and the files that change.
pub fn plan(root: &Path, level: Level) -> Result<(Version, Version, Vec<Change>), String> {
    let manifest_path = root.join("Cargo.toml");
    let manifest = read(&manifest_path)?;
    let old = workspace_version(&manifest)?;
    let new = old.bump(level);
    let mut changes = vec![Change { path: manifest_path, contents: bump_manifest(&manifest, new)? }];
    if old.requirement() != new.requirement() {
        for path in snippet_files(root) {
            let text = read(&path)?;
            let updated = bump_snippets(&text, &old.requirement(), &new.requirement());
            if updated != text {
                changes.push(Change { path, contents: updated });
            }
        }
    }
    Ok((old, new, changes))
}

pub fn workspace_version(manifest: &str) -> Result<Version, String> {
    let document: DocumentMut = manifest.parse().map_err(|error| format!("Cargo.toml: {error}"))?;
    let version = document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(|version| version.as_str())
        .ok_or("Cargo.toml has no [workspace.package] version")?;
    Version::parse(version)
}

/// The root manifest with the new version: `[workspace.package] version` and
/// the `version` of every `kardia*` entry in `[workspace.dependencies]`.
pub fn bump_manifest(manifest: &str, new: Version) -> Result<String, String> {
    let mut document: DocumentMut = manifest.parse().map_err(|error| format!("Cargo.toml: {error}"))?;
    let new = new.to_string();
    set_keeping_decor(&mut document["workspace"]["package"]["version"], &new);
    if let Some(dependencies) = document["workspace"]["dependencies"].as_table_like_mut() {
        for (name, entry) in dependencies.iter_mut() {
            if name.starts_with("kardia")
                && let Some(version) = entry.as_table_like_mut().and_then(|table| table.get_mut("version"))
            {
                set_keeping_decor(version, &new);
            }
        }
    }
    Ok(document.to_string())
}

/// Sets a string value, keeping the spaces and comment around the old one.
fn set_keeping_decor(item: &mut toml_edit::Item, new: &str) {
    let decor = item.as_value().map(|old| old.decor().clone());
    *item = value(new);
    if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
        *value.decor_mut() = decor;
    }
}

/// `text` with every `kardia = "<old>"` / `kardia-<crate> = "<old>"`
/// dependency requirement set to `new`.
pub fn bump_snippets(text: &str, old: &str, new: &str) -> String {
    let needle = format!(" = \"{old}\"");
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(&needle) {
        let (before, after) = rest.split_at(index);
        let crate_name = before.rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).next();
        result.push_str(before);
        if crate_name.is_some_and(|name| name == "kardia" || name.starts_with("kardia-")) {
            result.push_str(&format!(" = \"{new}\""));
        } else {
            result.push_str(&needle);
        }
        rest = &after[needle.len()..];
    }
    result.push_str(rest);
    result
}

/// Files that show users a dependency on kardia.
fn snippet_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![root.join("README.md")];
    if let Ok(entries) = std::fs::read_dir(root.join("site/content/docs")) {
        let mut pages: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "mdx"))
            .collect();
        pages.sort();
        files.extend(pages);
    }
    files.retain(|path| path.is_file());
    files
}

fn update_lockfile(root: &Path) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(root)
        .args(["update", "--workspace", "--offline", "--quiet"])
        .status()
        .map_err(|error| format!("cannot run cargo: {error}"))?;
    if status.success() { Ok(()) } else { Err("`cargo update --workspace` failed".into()) }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.1.3"  # the release version
edition = "2024"

[workspace.dependencies]
kardia-core = { path = "crates/kardia-core", version = "0.1.3" }
kardia-macros = { path = "crates/kardia-macros", version = "0.1.3" }
serde = { version = "1", features = ["derive"] }
tokio = "1.47"
"#;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn bumps_each_level() {
        assert_eq!(version("0.1.3").bump(Level::Patch), version("0.1.4"));
        assert_eq!(version("0.1.3").bump(Level::Minor), version("0.2.0"));
        assert_eq!(version("0.4.2").bump(Level::Major), version("1.0.0"));
        assert_eq!(version("1.2.3").bump(Level::Minor), version("1.3.0"));
    }

    #[test]
    fn refuses_what_is_not_major_minor_patch() {
        for text in ["0.1", "0.1.0-alpha", "1.2.3.4", "x.1.0", ""] {
            assert!(Version::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn requirement_follows_semver_compatibility() {
        assert_eq!(version("0.1.9").requirement(), "0.1");
        assert_eq!(version("1.4.0").requirement(), "1");
    }

    #[test]
    fn bumps_the_package_and_internal_dependency_versions_only() {
        assert_eq!(workspace_version(MANIFEST).unwrap(), version("0.1.3"));
        let bumped = bump_manifest(MANIFEST, version("0.2.0")).unwrap();
        assert_eq!(workspace_version(&bumped).unwrap(), version("0.2.0"));
        assert!(bumped.contains(r#"kardia-core = { path = "crates/kardia-core", version = "0.2.0" }"#));
        assert!(bumped.contains(r#"kardia-macros = { path = "crates/kardia-macros", version = "0.2.0" }"#));
        assert!(bumped.contains(r#"serde = { version = "1", features = ["derive"] }"#));
        assert!(bumped.contains(r#"tokio = "1.47""#));
        assert!(bumped.contains("# the release version"));
    }

    #[test]
    fn rewrites_kardia_requirements_and_nothing_else() {
        let text = "kardia = \"0.1\"\nkardia-guest = \"0.1\"\nserde = \"0.1\"\nmykardia = \"0.1\"\n";
        assert_eq!(
            bump_snippets(text, "0.1", "0.2"),
            "kardia = \"0.2\"\nkardia-guest = \"0.2\"\nserde = \"0.1\"\nmykardia = \"0.1\"\n"
        );
    }

    #[test]
    fn plans_the_files_of_a_workspace() {
        let root = scratch("plan");
        std::fs::write(root.join("Cargo.toml"), MANIFEST).unwrap();
        std::fs::write(root.join("README.md"), "kardia = \"0.1\"\n").unwrap();
        std::fs::create_dir_all(root.join("site/content/docs")).unwrap();
        std::fs::write(root.join("site/content/docs/a.mdx"), "kardia-guest = \"0.1\"\n").unwrap();
        std::fs::write(root.join("site/content/docs/b.mdx"), "no snippet\n").unwrap();

        let (old, new, changes) = plan(&root, Level::Patch).unwrap();
        assert_eq!((old, new), (version("0.1.3"), version("0.1.4")));
        let names = |changes: &[Change]| -> Vec<String> {
            changes.iter().map(|c| c.path.strip_prefix(&root).unwrap().display().to_string()).collect()
        };
        // a patch keeps the requirement users write
        assert_eq!(names(&changes), ["Cargo.toml"]);

        let (_, new, changes) = plan(&root, Level::Minor).unwrap();
        assert_eq!(new, version("0.2.0"));
        assert_eq!(names(&changes), ["Cargo.toml", "README.md", "site/content/docs/a.mdx"]);
        assert_eq!(changes[1].contents, "kardia = \"0.2\"\n");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-version-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
