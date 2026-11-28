//! `cargo xtask release`: plan a crates release from what changed since the
//! last one, bumping only the packages that need a new version.
//!
//! A package needs one when its own sources or manifest changed, or when a
//! package it publishes depends on changed: a dependent that keeps its old
//! version would keep pinning users to the old requirement. Tests, benches,
//! examples and prose do not reach users and never trigger a release.
//!
//! A minor or major release moves the requirement users write, so it releases
//! the whole workspace: every published crate would otherwise keep asking for
//! a version that does not exist.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, value};

use crate::git::git;
use crate::graph::{Node, graph};
use crate::version::{Level, Version};

#[derive(Debug)]
pub struct Options {
    pub level: Level,
    /// The packages whose changes to release, instead of asking git.
    pub only: Vec<String>,
    /// Whether to leave dependents on their current version.
    pub no_dependents: bool,
    /// The commit or tag to compare against.
    pub since: Option<String>,
    pub apply: bool,
    pub tag: Option<String>,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options =
            Self { level: Level::Patch, only: Vec::new(), no_dependents: false, since: None, apply: false, tag: None };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| args.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
            match arg.as_str() {
                "patch" => options.level = Level::Patch,
                "minor" => options.level = Level::Minor,
                "major" => options.level = Level::Major,
                "--only" => options.only.push(value("--only")?),
                "--no-dependents" => options.no_dependents = true,
                "--since" => options.since = Some(value("--since")?),
                "--tag" => options.tag = Some(value("--tag")?),
                "--apply" => options.apply = true,
                other => {
                    return Err(format!(
                        "unexpected argument `{other}`: release [patch|minor|major] \
                         [--only CRATE]... [--no-dependents] [--since REV] [--tag NAME] [--apply]"
                    ));
                }
            }
        }
        Ok(options)
    }
}

/// One package getting a new version.
#[derive(Debug)]
pub struct Bump {
    pub name: String,
    pub from: Version,
    pub to: Version,
    /// Why it is released: its own change, or the package that pulled it in.
    pub reason: String,
}

/// Where the release is written.
#[derive(Debug)]
pub struct Plan {
    pub bumps: Vec<Bump>,
    /// The manifests to rewrite, and the root manifest's dependency table.
    pub changes: Vec<Change>,
}

#[derive(Debug)]
pub struct Change {
    pub path: PathBuf,
    pub contents: String,
}

/// A whole release: what changed, what it releases, and where.
pub fn plan(root: &Path, options: &Options) -> Result<Plan, String> {
    let nodes = graph(root)?;
    let changed = if options.only.is_empty() {
        changed_packages(root, &nodes, options.since.as_deref())?
    } else {
        let known: BTreeSet<&str> = nodes.iter().map(|node| node.name.as_str()).collect();
        for name in &options.only {
            if !known.contains(name.as_str()) {
                return Err(format!("`{name}` is not a publishable workspace package"));
            }
        }
        options.only.iter().map(|name| (name.clone(), "asked for with --only".to_string())).collect()
    };
    if changed.is_empty() {
        return Err("no package changed since the last release: nothing to release".into());
    }
    let targets = propagate(&nodes, &changed, options.no_dependents);
    // A minor or major release changes the requirement users write (^X.Y), which
    // reaches every dependent of the crate that moved and so the whole workspace:
    // releasing only part of it would leave published crates asking for a major
    // they cannot find.
    let targets =
        if options.level == Level::Patch { targets } else { nodes.iter().map(|node| node.name.clone()).collect() };
    let mut bumps = Vec::new();
    for name in &targets {
        let node = nodes.iter().find(|node| &node.name == name).expect("targets come from the graph");
        let from = package_version(&node.manifest)?;
        let reason = changed
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("required by a {} release", level_name(options.level)));
        bumps.push(Bump { name: name.clone(), from, to: from.bump(options.level), reason });
    }
    let changes = rewrite(root, &bumps)?;
    Ok(Plan { bumps, changes })
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Patch => "patch",
        Level::Minor => "minor",
        Level::Major => "major",
    }
}

/// The packages whose sources or manifest changed since `since`, and why.
fn changed_packages(root: &Path, nodes: &[Node], since: Option<&str>) -> Result<BTreeMap<String, String>, String> {
    let base = match since {
        Some(revision) => revision.to_string(),
        None => last_release_tag(root)?,
    };
    let range = format!("{base}..HEAD");
    let files = git(root, &["diff", "--name-only", &range, "--", "crates"])?;
    let mut changed = BTreeMap::new();
    for file in files.lines().map(str::trim).filter(|line| !line.is_empty()) {
        // `crates/<directory>/<rest>`
        let mut parts = file.splitn(3, '/');
        if parts.next() != Some("crates") {
            continue;
        }
        let (Some(directory), Some(rest)) = (parts.next(), parts.next()) else { continue };
        // tests, benches, examples and prose never reach a user of the crate
        let counts = rest == "Cargo.toml" || rest.starts_with("src/");
        if !counts {
            continue;
        }
        let Some(node) = nodes
            .iter()
            .find(|node| node.manifest.parent().and_then(Path::file_name).is_some_and(|name| name == directory))
        else {
            continue;
        };
        changed.entry(node.name.clone()).or_insert_with(|| format!("changed since {base}"));
    }
    Ok(changed)
}

/// The newest `release-*` tag, which is what a release compares against.
fn last_release_tag(root: &Path) -> Result<String, String> {
    let tags = git(root, &["tag", "--list", "release-*", "--sort=-creatordate"])?;
    tags.lines()
        .map(str::trim)
        .find(|tag| !tag.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "there is no release-* tag to compare against: pass --since REV for the first release".into())
}

/// The changed packages, and every package whose published dependencies reach one.
fn propagate(nodes: &[Node], changed: &BTreeMap<String, String>, no_dependents: bool) -> BTreeSet<String> {
    let mut targets: BTreeSet<String> = changed.keys().cloned().collect();
    if no_dependents {
        return targets;
    }
    // a dev-dependency is not published, so it does not pull its user in
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in nodes {
        for edge in &node.edges {
            if edge.dev {
                continue;
            }
            dependents.entry(nodes[edge.to].name.as_str()).or_default().push(node.name.as_str());
        }
    }
    loop {
        let mut grew = false;
        for name in targets.clone() {
            for dependent in dependents.get(name.as_str()).into_iter().flatten() {
                if targets.insert((*dependent).to_string()) {
                    grew = true;
                }
            }
        }
        if !grew {
            return targets;
        }
    }
}

/// Reads `[package] version` from a manifest.
pub fn package_version(manifest: &Path) -> Result<Version, String> {
    let text = read(manifest)?;
    let document: DocumentMut = text.parse().map_err(|error| format!("{}: {error}", manifest.display()))?;
    let version = document["package"]["version"]
        .as_str()
        .ok_or_else(|| format!("{}: [package] has no version", manifest.display()))?;
    Version::parse(version)
}

/// Every manifest with its package's new version, and the root manifest with
/// the new lower bounds its dependency table names.
fn rewrite(root: &Path, bumps: &[Bump]) -> Result<Vec<Change>, String> {
    let mut changes = Vec::new();
    let mut root_manifest = read(&root.join("Cargo.toml"))?;
    for bump in bumps {
        let manifest = root.join("crates").join(&bump.name).join("Cargo.toml");
        let text = read(&manifest)?;
        changes.push(Change { path: manifest, contents: set_package_version(&text, bump.to)? });
        root_manifest = set_dependency_version(&root_manifest, &bump.name, bump.from, bump.to)?;
    }
    changes.push(Change { path: root.join("Cargo.toml"), contents: root_manifest });
    Ok(changes)
}

fn set_package_version(manifest: &str, new: Version) -> Result<String, String> {
    let mut document: DocumentMut = manifest.parse().map_err(|error| format!("Cargo.toml: {error}"))?;
    let item = &mut document["package"]["version"];
    let decor = item.as_value().map(|old| old.decor().clone());
    *item = value(new.to_string());
    if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
        *value.decor_mut() = decor;
    }
    Ok(document.to_string())
}

/// Raises `[workspace.dependencies] <crate>` to `new` when it names `from`.
/// A bound below the released version already accepts it, so it stays.
fn set_dependency_version(manifest: &str, name: &str, from: Version, new: Version) -> Result<String, String> {
    let mut document: DocumentMut = manifest.parse().map_err(|error| format!("Cargo.toml: {error}"))?;
    let Some(dependencies) = document["workspace"]["dependencies"].as_table_like_mut() else {
        return Ok(manifest.to_string());
    };
    let Some(entry) = dependencies.get_mut(name) else { return Ok(document.to_string()) };
    let Some(version) = entry.as_table_like_mut().and_then(|table| table.get_mut("version")) else {
        return Ok(document.to_string());
    };
    if version.as_str().is_some_and(|text| text == from.to_string()) {
        let decor = version.as_value().map(|old| old.decor().clone());
        *version = value(new.to_string());
        if let (Some(decor), Some(value)) = (decor, version.as_value_mut()) {
            *value.decor_mut() = decor;
        }
    }
    Ok(document.to_string())
}

/// The batch tag a release gets: `release-<date>`, or the one asked for.
pub fn tag_name(date: &str, options: &Options) -> String {
    options.tag.clone().unwrap_or_else(|| format!("release-{date}"))
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// What the plan prints: the packages, why, and the versions they move to.
pub fn describe(plan: &Plan) -> String {
    let mut text = String::new();
    let width = plan.bumps.iter().map(|bump| bump.name.len()).max().unwrap_or(0);
    for bump in &plan.bumps {
        text.push_str(&format!(
            "  {:<width$}  {} -> {}  ({})\n",
            bump.name,
            bump.from,
            bump.to,
            bump.reason,
            width = width
        ));
    }
    text
}

/// Writes the plan, updates the lockfile, and tags the batch. Nothing is
/// pushed: the tag names a commit the working tree no longer has.
pub fn apply(root: &Path, plan: &Plan, options: &Options, date: &str) -> Result<(), String> {
    for change in &plan.changes {
        std::fs::write(&change.path, &change.contents)
            .map_err(|error| format!("{}: {error}", change.path.display()))?;
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = std::process::Command::new(cargo)
        .current_dir(root)
        .args(["update", "--workspace", "--offline", "--quiet"])
        .status()
        .map_err(|error| format!("cannot run cargo: {error}"))?;
    if !status.success() {
        return Err("`cargo update --workspace` failed".into());
    }
    let tag = tag_name(date, options);
    if crate::git::git(root, &["tag", "--list", &tag])?.trim() == tag {
        return Err(format!("tag {tag} already exists"));
    }
    let message = format!("kardia release {date}: {}", summary(plan));
    crate::git::git(root, &["tag", "--annotate", &tag, "--message", &message])?;
    println!("Wrote {} manifests and tagged {tag}.", plan.changes.len());
    println!("Commit the manifests, then move the tag: `git tag -f {tag} && git push --follow-tags`.");
    Ok(())
}

/// Prints the plan; `--apply` writes it. Releasing is not reversible, so the
/// default changes nothing.
pub fn run(root: &Path, options: &Options) -> Result<(), String> {
    let plan = plan(root, options)?;
    let date = today(root)?;
    println!("Release {}", tag_name(&date, options));
    print!("{}", describe(&plan));
    for change in &plan.changes {
        println!("  {}", change.path.strip_prefix(root).unwrap_or(&change.path).display());
    }
    if options.apply {
        return apply(root, &plan, options, &date);
    }
    println!("Dry run: nothing written. Pass --apply to write these and tag the release.");
    Ok(())
}

/// The commit date of `HEAD` as `YYYY-MM-DD`: the tag should name when the
/// release was cut, not when it is applied to a working tree.
fn today(root: &Path) -> Result<String, String> {
    let date = git(root, &["log", "-1", "--format=%cs"])?;
    let date = date.trim();
    if date.len() == 10 && date.as_bytes()[4] == b'-' {
        Ok(date.to_string())
    } else {
        Err(format!("`{date}` is not a commit date"))
    }
}

/// `kardia-core 0.3.1, kardia-hmr 0.3.1` for the tag message.
fn summary(plan: &Plan) -> String {
    plan.bumps.iter().map(|bump| format!("{} {}", bump.name, bump.to)).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use crate::graph::Edge;

    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn options(args: &[&str]) -> Options {
        Options::parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()).unwrap()
    }

    /// Three packages, sorted as Cargo names them: `app` -> `core`
    /// -> `timer`, plus `timer`'s test-only `app`, so a change to `app`
    /// reaches only itself.
    fn nodes() -> Vec<Node> {
        let mut nodes: Vec<Node> = ["app", "core", "timer"]
            .into_iter()
            .map(|name| Node {
                name: name.into(),
                manifest: PathBuf::from(format!("/repo/crates/{name}/Cargo.toml")),
                edges: Vec::new(),
            })
            .collect();
        let edge = |to: usize, dev: bool| Edge {
            to,
            section: "dependencies".into(),
            key: String::new(),
            inherited: false,
            optional: false,
            dev,
        };
        nodes[0].edges.push(edge(1, false));
        nodes[1].edges.push(edge(2, false));
        nodes[2].edges.push(edge(0, true));
        nodes
    }

    fn changed(names: &[&str]) -> BTreeMap<String, String> {
        names.iter().map(|name| ((*name).to_string(), "changed".to_string())).collect()
    }

    #[test]
    fn defaults_to_a_dry_run_patch_against_the_last_release_tag() {
        let plain = options(&[]);
        assert_eq!(plain.level, Level::Patch);
        assert!(!plain.apply);
        assert!(!plain.no_dependents);
        assert_eq!(plain.since, None);
        assert!(options(&["--apply"]).apply);
    }

    #[test]
    fn propagates_to_every_published_dependent_but_not_through_dev_edges() {
        let nodes = nodes();
        let targets = propagate(&nodes, &changed(&["app"]), false);
        assert_eq!(targets, BTreeSet::from(["app".to_string()]), "only a dev edge reaches app");

        let targets = propagate(&nodes, &changed(&["core"]), false);
        assert_eq!(
            targets.into_iter().collect::<Vec<_>>(),
            ["app", "core"],
            "core pulls app; timer's dev edge to app does not"
        );

        let targets = propagate(&nodes, &changed(&["core"]), true);
        assert_eq!(targets.into_iter().collect::<Vec<_>>(), ["core"]);
    }

    #[test]
    fn reads_and_raises_versions_keeping_the_manifests_layout() {
        let manifest = "[package]\nname = \"kardia-core\"\nversion = \"0.3.4\"  # release\n";
        let bumped = set_package_version(manifest, version("0.3.5")).unwrap();
        assert_eq!(bumped, "[package]\nname = \"kardia-core\"\nversion = \"0.3.5\"  # release\n");

        let root = "[workspace.dependencies]\nkardia-core = { path = \"crates/kardia-core\", version = \"0.3.4\" }\n\
                    kardia-timer = { path = \"crates/kardia-timer\", version = \"0.3.4\" }\nserde = \"1\"\n";
        let raised = set_dependency_version(root, "kardia-core", version("0.3.4"), version("0.3.5")).unwrap();
        assert!(raised.contains(r#"kardia-core = { path = "crates/kardia-core", version = "0.3.5" }"#), "{raised}");
        assert!(raised.contains(r#"kardia-timer = { path = "crates/kardia-timer", version = "0.3.4" }"#), "untouched");
        assert!(raised.contains("serde = \"1\""));

        let stale = set_dependency_version(root, "kardia-core", version("0.2.0"), version("0.3.5")).unwrap();
        assert_eq!(stale, root, "a bound that already accepts the new version stays");
    }

    #[test]
    fn names_the_batch_after_its_date_unless_asked_otherwise() {
        assert_eq!(tag_name("2026-11-28", &options(&[])), "release-2026-11-28");
        assert_eq!(tag_name("2026-11-28", &options(&["--tag", "release-rc1"])), "release-rc1");
    }

    #[test]
    fn describes_every_package_with_its_reason() {
        let plan = Plan {
            bumps: vec![
                Bump {
                    name: "kardia-core".into(),
                    from: version("0.3.0"),
                    to: version("0.3.1"),
                    reason: "changed".into(),
                },
                Bump {
                    name: "kardia-hmr".into(),
                    from: version("0.3.0"),
                    to: version("0.3.1"),
                    reason: "required by a changed dependency".into(),
                },
            ],
            changes: Vec::new(),
        };
        assert_eq!(
            describe(&plan),
            "  kardia-core  0.3.0 -> 0.3.1  (changed)\n  kardia-hmr   0.3.0 -> 0.3.1  (required by a changed dependency)\n"
        );
        assert_eq!(summary(&plan), "kardia-core 0.3.1, kardia-hmr 0.3.1");
    }

    #[test]
    fn applies_the_plan_and_tags_the_batch_in_a_real_repository() {
        let fixture = Fixture::new("apply");
        let options = options(&["--only", "kardia-core", "--apply", "--tag", "release-test"]);
        let planned = plan(&fixture.root, &options).unwrap();
        apply(&fixture.root, &planned, &options, "2026-11-28").unwrap();

        let manifest = std::fs::read_to_string(fixture.root.join("crates/kardia-core/Cargo.toml")).unwrap();
        assert!(manifest.contains("version = \"0.3.1\""), "{manifest}");
        assert_eq!(crate::git::git(&fixture.root, &["tag", "--list"]).unwrap().trim(), "release-test");

        let again = apply(&fixture.root, &planned, &options, "2026-11-28").unwrap_err();
        assert!(again.contains("already exists"), "{again}");
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("xtask-release-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("crates/kardia-core/src")).unwrap();
            std::fs::write(root.join("crates/kardia-core/src/lib.rs"), "").unwrap();
            std::fs::write(
                root.join("crates/kardia-core/Cargo.toml"),
                "[package]\nname = \"kardia-core\"\nversion = \"0.3.0\"\nedition = \"2024\"\n",
            )
            .unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                "[workspace]\nmembers = [\"crates/kardia-core\"]\nresolver = \"3\"\n\n\
                 [workspace.dependencies]\nkardia-core = { path = \"crates/kardia-core\", version = \"0.3.0\" }\n",
            )
            .unwrap();
            std::fs::write(root.join("Cargo.lock"), "").unwrap();
            for args in [
                vec!["init", "--quiet"],
                vec!["config", "user.name", "xtask"],
                vec!["config", "user.email", "xtask@example.com"],
                vec!["config", "commit.gpgsign", "false"],
                vec!["config", "tag.gpgsign", "false"],
                vec!["add", "."],
                vec!["commit", "--quiet", "-m", "init"],
            ] {
                let args: Vec<&str> = args.to_vec();
                crate::git::git(&root, &args).unwrap();
            }
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
