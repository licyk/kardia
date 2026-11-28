//! Repository tasks: `cargo xtask <command>`.

mod arch;
mod cli;
mod deps;
mod git;
mod graph;
mod js;
mod publish_check;
mod release;
mod tag;
mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Task};

fn main() -> ExitCode {
    match run(&root(), Cli::parse().task) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(root: &Path, task: Task) -> Result<(), String> {
    match task {
        Task::Arch => arch::run(root),
        Task::PublishCheck => publish_check::run(root, publish_check::Scope::Published),
        Task::CycleCheck => publish_check::run(root, publish_check::Scope::Workspace),
        Task::Update { dry_run } => deps::update(root, &deps::Options { dry_run }),
        Task::Lock { dry_run } => deps::lock(root, &deps::Options { dry_run }),
        Task::JsVersion { level, dry_run } => js::run(root, &js::Options { level, dry_run }),
        Task::JsTag { version, name, message, push, remote, dry_run } => {
            let options = tag::Options { version, name, message, push, remote, dry_run };
            tag::run(root, &options, &js::release(root)?)
        }
        Task::Release { level, only, no_dependents, since, tag, apply } => {
            release::run(root, &release::Options { level, only, no_dependents, since, apply, tag })
        }
    }
}

/// The workspace root: the parent of this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace").to_path_buf()
}
