//! Checks cycles between workspace packages in Cargo's published dependency metadata.

use std::path::Path;

use crate::graph::{Node, graph};

pub fn run(root: &Path) -> Result<(), String> {
    let graph = graph(root)?;
    if let Some(cycle) = cycle(&graph) {
        return Err(describe_cycle(root, &graph, &cycle));
    }
    let edges: usize = graph.iter().map(|node| node.edges.len()).sum();
    println!("No publishing dependency cycles: {} publishable workspace packages, {edges} dependencies.", graph.len());
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
