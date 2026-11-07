//! Repository tasks: `cargo xtask <command>`.

mod arch;
mod git;
mod js;
mod tag;
mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: cargo xtask <command>

Commands:
  arch                                 Check that lib.rs and mod.rs files only declare and re-export modules
  version <patch|minor|major>          Bump the workspace version everywhere it is named
          [--dry-run]
  tag [--version X.Y.Z | --name NAME]  Create an annotated tag on HEAD (default: v<workspace version>)
      [--message TEXT] [--push] [--remote NAME] [--dry-run]
  js-version <patch|minor|major>       Bump the version of the npm packages under js/
             [--dry-run]
  js-tag [same options as tag]         Tag the npm release (default: js-v<npm packages version>)";

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
        "version" => version::run(root, &version::Options::parse(rest)?),
        "tag" => tag::run(root, &tag::Options::parse(rest)?, &tag::Release::crates(root)?),
        "js-version" => js::run(root, &version::Options::parse(rest)?),
        "js-tag" => tag::run(root, &tag::Options::parse(rest)?, &js::release(root)?),
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
