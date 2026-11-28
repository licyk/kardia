//! The command line: `cargo xtask <command>`, parsed by clap.

use clap::{Parser, Subcommand};

pub use crate::version::Level;

#[derive(Debug, Parser)]
#[command(name = "cargo xtask", bin_name = "cargo xtask", about = "Repository tasks for kardia")]
pub struct Cli {
    #[command(subcommand)]
    pub task: Task,
}

#[derive(Debug, Subcommand)]
pub enum Task {
    /// Check that lib.rs and mod.rs files only declare and re-export modules.
    Arch,
    /// Check workspace publishing dependencies for cycles (offline).
    PublishCheck,
    /// Check every workspace dependency for cycles, dev-dependencies too.
    CycleCheck,
    /// Raise Cargo.lock to the newest versions the manifests allow.
    Update {
        /// Print the versions that would change without writing Cargo.lock.
        #[arg(long)]
        dry_run: bool,
    },
    /// Rewrite Cargo.lock to agree with the manifests, keeping locked versions.
    Lock {
        /// Print what would change without writing Cargo.lock.
        #[arg(long)]
        dry_run: bool,
    },
    /// Bump the version of the npm packages under js/.
    JsVersion {
        #[arg(value_enum)]
        level: Level,
        /// Print the files that would change without writing them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Create an annotated tag on HEAD for @kardiajs (default: js-v<version>).
    JsTag {
        /// The version the tag names; it must be the npm packages' version.
        #[arg(long, value_name = "X.Y.Z", conflicts_with = "name")]
        version: Option<String>,
        /// Any other tag name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[arg(long, short, value_name = "TEXT")]
        message: Option<String>,
        /// Push the tag to the remote.
        #[arg(long)]
        push: bool,
        #[arg(long, value_name = "NAME")]
        remote: Option<String>,
        /// Print what would be tagged without creating it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Plan a crates release for the packages that changed.
    Release {
        #[arg(value_enum, default_value = "patch")]
        level: Level,
        /// The packages whose changes to release, instead of asking git.
        #[arg(long, value_name = "CRATE")]
        only: Vec<String>,
        /// Leave dependents on their current version.
        #[arg(long)]
        no_dependents: bool,
        /// The commit or tag to compare against.
        #[arg(long, value_name = "REV")]
        since: Option<String>,
        /// The batch tag's name.
        #[arg(long, value_name = "NAME")]
        tag: Option<String>,
        /// Write the versions, update Cargo.lock and tag the batch.
        #[arg(long)]
        apply: bool,
    },
}
