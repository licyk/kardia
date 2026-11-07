//! `cargo xtask js-version <patch|minor|major>`: bumps the version of the npm
//! packages under `js/`, which are released together and apart from the
//! crates, and the `^` ranges they require of each other.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::tag::Release;
use crate::version::{Change, Level, Options, Version};

/// The packages, in dependency order.
pub const PACKAGES: [&str; 5] = ["core", "timer", "bridge", "tauri", "vue"];

pub fn run(root: &Path, options: &Options) -> Result<(), String> {
    let (old, new, changes) = plan(root, options.level)?;
    println!("@kardiajs {old} -> {new}");
    for change in &changes {
        println!("  {}", change.path.strip_prefix(root).unwrap_or(&change.path).display());
    }
    if options.dry_run {
        println!("  js/bun.lock (bun install)");
        return Ok(());
    }
    for change in &changes {
        std::fs::write(&change.path, &change.contents)
            .map_err(|error| format!("{}: {error}", change.path.display()))?;
    }
    update_lockfile(root)?;
    println!("  js/bun.lock");
    Ok(())
}

/// The npm release: `js-v<version>` tags.
pub fn release(root: &Path) -> Result<Release, String> {
    Ok(Release {
        prefix: "js-v",
        product: "@kardiajs",
        what: "npm packages",
        bump: "js-version",
        version: version(root)?,
    })
}

/// The version every package has; they must agree.
pub fn version(root: &Path) -> Result<Version, String> {
    let mut found: Option<Version> = None;
    for path in manifests(root) {
        let version = package_version(&read(&path)?).map_err(|error| format!("{}: {error}", path.display()))?;
        match found {
            Some(other) if other != version => {
                return Err(format!("{} is at {version} while other packages are at {other}", path.display()));
            }
            _ => found = Some(version),
        }
    }
    found.ok_or_else(|| "no npm packages found".into())
}

pub fn plan(root: &Path, level: Level) -> Result<(Version, Version, Vec<Change>), String> {
    let old = version(root)?;
    let new = old.bump(level);
    let changes = manifests(root)
        .into_iter()
        .map(|path| Ok(Change { contents: bump_package(&read(&path)?, old, new)?, path }))
        .collect::<Result<_, String>>()?;
    Ok((old, new, changes))
}

/// The manifest with `version` set to `new`, and each `"@kardiajs/<name>": "^<req>"`
/// range moved along when the requirement changes.
pub fn bump_package(text: &str, old: Version, new: Version) -> Result<String, String> {
    let (old_field, new_field) = (format!("\"version\": \"{old}\""), format!("\"version\": \"{new}\""));
    if !text.contains(&old_field) {
        return Err(format!("no {old_field} to bump"));
    }
    let mut text = text.replacen(&old_field, &new_field, 1);
    if old.requirement() != new.requirement() {
        for name in PACKAGES {
            let range = |version: Version| format!("\"@kardiajs/{name}\": \"^{}\"", version.requirement());
            text = text.replace(&range(old), &range(new));
        }
    }
    Ok(text)
}

fn package_version(text: &str) -> Result<Version, String> {
    let start = text.find("\"version\": \"").ok_or("no version")? + "\"version\": \"".len();
    let end = text[start..].find('"').ok_or("unterminated version")?;
    Version::parse(&text[start..start + end])
}

fn manifests(root: &Path) -> Vec<PathBuf> {
    PACKAGES.iter().map(|name| root.join("js/packages").join(name).join("package.json")).collect()
}

fn update_lockfile(root: &Path) -> Result<(), String> {
    let status = Command::new("bun")
        .current_dir(root.join("js"))
        .args(["install", "--silent"])
        .status()
        .map_err(|error| format!("cannot run bun: {error}"))?;
    if status.success() { Ok(()) } else { Err("`bun install` in js/ failed".into()) }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    const BRIDGE: &str = r#"{
  "name": "@kardiajs/bridge",
  "version": "0.1.4",
  "peerDependencies": {
    "@kardiajs/core": "^0.1",
    "@tauri-apps/api": "^0.1"
  }
}
"#;

    #[test]
    fn bumps_the_version_and_moves_internal_ranges_with_the_requirement() {
        let patched = bump_package(BRIDGE, version("0.1.4"), version("0.1.5")).unwrap();
        assert!(patched.contains(r#""version": "0.1.5""#));
        assert!(patched.contains(r#""@kardiajs/core": "^0.1""#), "a patch keeps the range");

        let minor = bump_package(BRIDGE, version("0.1.4"), version("0.2.0")).unwrap();
        assert!(minor.contains(r#""version": "0.2.0""#));
        assert!(minor.contains(r#""@kardiajs/core": "^0.2""#));
        assert!(minor.contains(r#""@tauri-apps/api": "^0.1""#), "external ranges stay");
    }

    #[test]
    fn refuses_a_manifest_at_another_version() {
        assert!(bump_package(BRIDGE, version("0.1.3"), version("0.1.4")).is_err());
    }

    #[test]
    fn plans_every_package_and_requires_them_to_agree() {
        let root = std::env::temp_dir().join(format!("xtask-js-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for name in PACKAGES {
            let dir = root.join("js/packages").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("package.json"), BRIDGE.replace("bridge", name)).unwrap();
        }
        let (old, new, changes) = plan(&root, Level::Minor).unwrap();
        assert_eq!((old, new), (version("0.1.4"), version("0.2.0")));
        assert_eq!(changes.len(), 5);
        assert_eq!(release(&root).unwrap().version, version("0.1.4"));

        let timer = root.join("js/packages/timer/package.json");
        std::fs::write(&timer, BRIDGE.replace("0.1.4", "0.1.9")).unwrap();
        let error = super::version(&root).unwrap_err();
        assert!(error.contains("0.1.9"), "{error}");
    }
}
