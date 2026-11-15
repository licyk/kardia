//! Checks cycles between workspace packages in Cargo's published dependency metadata.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use toml_edit::{DocumentMut, Item};

pub fn run(root: &Path) -> Result<(), String> {
    let graph = graph(root)?;
    if let Some(cycle) = cycle(&graph) {
        return Err(describe_cycle(root, &graph, &cycle));
    }
    let edges: usize = graph.iter().map(|node| node.edges.len()).sum();
    println!("No publishing dependency cycles: {} publishable workspace packages, {edges} dependencies.", graph.len());
    Ok(())
}

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

struct Node {
    name: String,
    manifest: PathBuf,
    edges: Vec<Edge>,
}

struct Edge {
    to: usize,
    section: String,
    key: String,
    inherited: bool,
    optional: bool,
}

fn graph(root: &Path) -> Result<Vec<Node>, String> {
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
        });
    }
    Ok(())
}

fn cycle(nodes: &[Node]) -> Option<Vec<(usize, usize)>> {
    let mut state = vec![0; nodes.len()];
    let mut path = Vec::new();
    for node in 0..nodes.len() {
        if state[node] == 0
            && let Some(cycle) = visit(node, nodes, &mut state, &mut path)
        {
            return Some(cycle);
        }
    }
    None
}

fn visit(node: usize, nodes: &[Node], state: &mut [u8], path: &mut Vec<(usize, usize)>) -> Option<Vec<(usize, usize)>> {
    state[node] = 1;
    for (index, edge) in nodes[node].edges.iter().enumerate() {
        path.push((node, index));
        if state[edge.to] == 1 {
            let start = path.iter().position(|(from, _)| *from == edge.to).unwrap();
            return Some(path[start..].to_vec());
        }
        if state[edge.to] == 0
            && let Some(cycle) = visit(edge.to, nodes, state, path)
        {
            return Some(cycle);
        }
        path.pop();
    }
    state[node] = 2;
    None
}

fn describe_cycle(root: &Path, nodes: &[Node], cycle: &[(usize, usize)]) -> String {
    let mut message = String::from("publishing dependency cycle:");
    for &(from, index) in cycle {
        let node = &nodes[from];
        let edge = &node.edges[index];
        let manifest = node.manifest.strip_prefix(root).unwrap_or(&node.manifest);
        message.push_str(&format!(
            "\n  {} -> {}\n    {} [{}].{}{}{}",
            node.name,
            nodes[edge.to].name,
            manifest.display(),
            edge.section,
            edge.key,
            if edge.inherited { " (version inherited from workspace.dependencies)" } else { "" },
            if edge.optional { " (optional)" } else { "" },
        ));
    }
    message
        .push_str("\nBreak the publishing cycle; repository-only test dependencies can use a path without a version.");
    message
}

fn read(path: &Path) -> Result<DocumentMut, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .parse()
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("kardia-publish-check-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for name in ["server", "plugin"] {
                std::fs::create_dir_all(root.join(name).join("src")).unwrap();
                std::fs::write(root.join(name).join("src/lib.rs"), "").unwrap();
            }
            let fixture = Self(root);
            fixture.workspace(true);
            fixture.package("plugin", "[dependencies]\ndemo-server = { path = '../server', version = '0.1' }\n", true);
            fixture
        }

        fn workspace(&self, versioned: bool) {
            std::fs::write(self.0.join("Cargo.toml"), format!(
                "[workspace]\nmembers = ['server', 'plugin']\nresolver = '3'\n[workspace.dependencies]\ndemo-plugin = {{ path = 'plugin'{} }}\n",
                if versioned { ", version = '0.1'" } else { "" },
            )).unwrap();
        }

        fn package(&self, name: &str, dependencies: &str, publish: bool) {
            std::fs::write(self.0.join(name).join("Cargo.toml"), format!(
                "[package]\nname = 'demo-{name}'\nversion = '0.1.0'\nedition = '2024'\npublish = {publish}\n{dependencies}",
            )).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn checks_inherited_and_explicit_versions_but_omits_path_only_dev_dependencies() {
        let fixture = Fixture::new("versions");
        for declaration in ["demo-plugin.workspace = true", "demo-plugin = { path = '../plugin', version = '*' }"] {
            fixture.package("server", &format!("[dev-dependencies]\n{declaration}\n"), true);
            let error = run(&fixture.0).unwrap_err();
            assert!(error.contains("demo-server -> demo-plugin"), "{error}");
            assert!(error.contains("demo-plugin -> demo-server"), "{error}");
            assert!(error.contains("[dev-dependencies].demo-plugin"), "{error}");
            if declaration.contains("workspace") {
                assert!(error.contains("inherited from workspace.dependencies"));
            }
        }
        fixture.package("server", "[dev-dependencies]\ndemo-plugin = { path = '../plugin' }\n", true);
        run(&fixture.0).unwrap();
        fixture.workspace(false);
        fixture.package("server", "[dev-dependencies]\ndemo-plugin.workspace = true\n", true);
        run(&fixture.0).unwrap();
        assert!(!fixture.0.join("Cargo.lock").exists());
        assert!(!fixture.0.join("target").exists());
    }

    #[test]
    fn reports_renames_optional_dependencies_and_non_host_targets() {
        let fixture = Fixture::new("target");
        fixture.package("server", "[target.'cfg(windows)'.dependencies]\nalias = { package = 'demo-plugin', path = '../plugin', version = '0.1', optional = true }\n", true);
        let error = run(&fixture.0).unwrap_err();
        assert!(error.contains("demo-server -> demo-plugin"), "{error}");
        assert!(error.contains("[target.'cfg(windows)'.dependencies].alias"), "{error}");
        assert!(error.contains("(optional)"), "{error}");
    }

    #[test]
    fn checks_build_dependencies_and_excludes_unpublished_workspace_members() {
        let fixture = Fixture::new("build");
        fixture.package("server", "[build-dependencies]\ndemo-plugin.workspace = true\n", true);
        assert!(run(&fixture.0).unwrap_err().contains("[build-dependencies].demo-plugin"));
        fixture.package("server", "[build-dependencies]\ndemo-plugin.workspace = true\n", false);
        run(&fixture.0).unwrap();
    }
}
