//! Repository tasks: `cargo xtask <command>`.

mod arch;
mod git;
mod graph;
mod js;
mod publish_check;
mod release;
mod tag;
mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: cargo xtask <command>

Commands:
  arch                                 Check that lib.rs and mod.rs files only declare and re-export modules
  publish-check                        Check workspace publishing dependencies for cycles (offline)
  cycle-check                          Check every workspace dependency for cycles, dev-dependencies too
  js-version <patch|minor|major>       Bump the version of the npm packages under js/
             [--dry-run]
  js-tag [--version X.Y.Z | --name NAME]
         [--message TEXT] [--push] [--remote NAME] [--dry-run]
                                       Create an annotated tag on HEAD for @kardiajs
                                       (default: js-v<npm packages version>)
  release [patch|minor|major]          Plan a crates release for the packages that changed
          [--only CRATE]... [--no-dependents] [--since REV] [--tag NAME] [--apply]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&root(), &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let (command, rest) = args.split_first().ok_or_else(|| USAGE.to_string())?;
    match command.as_str() {
        "arch" => arch::run(root),
        "publish-check" if rest.is_empty() => publish_check::run(root, publish_check::Scope::Published),
        "publish-check" => Err("usage: cargo xtask publish-check".into()),
        "cycle-check" if rest.is_empty() => publish_check::run(root, publish_check::Scope::Workspace),
        "cycle-check" => Err("usage: cargo xtask cycle-check".into()),
        "js-version" => js::run(root, &js::Options::parse(rest)?),
        "js-tag" => tag::run(root, &tag::Options::parse(rest)?, &js::release(root)?),
        "release" => release::run(root, &release::Options::parse(rest)?),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

/// The workspace root: the parent of this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace").to_path_buf()
}
