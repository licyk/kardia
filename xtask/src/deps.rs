//! `cargo xtask update` and `cargo xtask lock`: the workspace's `Cargo.lock`,
//! brought up to the newest versions its manifests allow, or brought into
//! agreement with them without upgrading what is already locked.
//!
//! Neither touches a manifest: the requirement a crate writes is its own, and
//! raising it is a deliberate edit, not a side effect of refreshing the lock.

use std::path::Path;
use std::process::Command;

#[derive(Debug, Default, clap::Args)]
pub struct Options {
    /// Print the versions that would change without writing Cargo.lock.
    #[arg(long)]
    pub dry_run: bool,
}

/// `cargo xtask update`: every package that is not a workspace member moves to
/// the newest version its requirement allows.
pub fn update(root: &Path, options: &Options) -> Result<(), String> {
    cargo_update(root, options, &["--workspace"])
}

/// `cargo xtask lock`: `Cargo.lock` rewritten to agree with the manifests,
/// keeping every version already locked, and written from scratch when it is
/// missing — there is nothing to keep then, so it holds the newest allowed.
pub fn lock(root: &Path, options: &Options) -> Result<(), String> {
    if !root.join("Cargo.lock").exists() {
        println!("Cargo.lock is missing: it is written with the newest versions allowed");
    }
    cargo_update(root, options, &[])
}

fn cargo_update(root: &Path, options: &Options, selection: &[&str]) -> Result<(), String> {
    let mut command = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    command.current_dir(root).arg("update").args(selection);
    if options.dry_run {
        command.arg("--dry-run");
    }
    let output = command.output().map_err(|error| format!("cannot run `cargo update`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo update {}` failed: {}",
            selection.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // `cargo update` says what it did on stderr; a dry run would otherwise
    // report nothing at all.
    if options.dry_run {
        print!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}
