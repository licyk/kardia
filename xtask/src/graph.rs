//! The workspace's packages and how they depend on each other in what Cargo
//! publishes: a package, the packages it names, and how each edge is declared.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use toml_edit::{DocumentMut, Item};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    publish: Option<Vec<String>>,
}

pub struct Node {
    pub name: String,
    pub manifest: PathBuf,
    pub edges: Vec<Edge>,
}

pub struct Edge {
    pub to: usize,
    pub section: String,
    pub key: String,
    pub inherited: bool,
    pub optional: bool,
    /// A dependency only the tests see: it does not bind the published package.
    pub dev: bool,
}

/// The publishable workspace members, sorted by name, with their edges.
pub fn graph(root: &Path) -> Result<Vec<Node>, String> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1", "--offline", "--locked"])
        .output()
        .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    let workspace = read(&metadata.workspace_root.join("Cargo.toml"))?;
    let mut nodes: Vec<Node> = metadata
        .packages
        .into_iter()
        .filter(|package| {
            metadata.workspace_members.contains(&package.id)
                && package.publish.as_ref().is_none_or(|registries| !registries.is_empty())
        })
        .map(|package| Node { name: package.name, manifest: package.manifest_path, edges: Vec::new() })
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut directories = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        directories.insert(node.manifest.parent().unwrap().canonicalize().map_err(|error| error.to_string())?, index);
    }
    for node in &mut nodes {
        let document = read(&node.manifest)?;
        for kind in ["dependencies", "build-dependencies", "dev-dependencies"] {
            add_edges(node, document.as_item(), kind, kind, &workspace, &metadata.workspace_root, &directories)?;
            if let Some(targets) = document.get("target").and_then(Item::as_table_like) {
                for (target, table) in targets.iter() {
                    add_edges(
                        node,
                        table,
                        kind,
                        &format!("target.'{target}'.{kind}"),
                        &workspace,
                        &metadata.workspace_root,
                        &directories,
                    )?;
                }
            }
        }
    }
    Ok(nodes)
}

fn add_edges(
    node: &mut Node,
    table: &Item,
    kind: &str,
    section: &str,
    workspace: &DocumentMut,
    workspace_root: &Path,
    directories: &HashMap<PathBuf, usize>,
) -> Result<(), String> {
    let Some(dependencies) = table.get(kind).and_then(Item::as_table_like) else { return Ok(()) };
    for (key, declaration) in dependencies.iter() {
        let inherited = declaration.get("workspace").and_then(Item::as_bool) == Some(true);
        let (dependency, base) = if inherited {
            (&workspace["workspace"]["dependencies"][key], workspace_root)
        } else {
            (declaration, node.manifest.parent().unwrap())
        };
        // Metadata reports both omitted versions and explicit "*" as "*"; only
        // the original declaration tells whether Cargo retains a dev-dependency.
        if kind == "dev-dependencies" && dependency.get("version").is_none() && !dependency.is_str() {
            continue;
        }
        let Some(path) = dependency.get("path").and_then(Item::as_str) else { continue };
        let directory = base.join(path).canonicalize().map_err(|error| format!("{section}.{key}: {error}"))?;
        let Some(&to) = directories.get(&directory) else { continue };
        node.edges.push(Edge {
            to,
            section: section.into(),
            key: key.into(),
            inherited,
            optional: declaration.get("optional").and_then(Item::as_bool) == Some(true),
            dev: kind == "dev-dependencies",
        });
    }
    Ok(())
}

fn read(path: &Path) -> Result<DocumentMut, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .parse()
        .map_err(|error| format!("{}: {error}", path.display()))
}
