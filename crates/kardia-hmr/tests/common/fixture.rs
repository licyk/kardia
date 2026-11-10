//! The Rust guest fixture (`fixtures/rust-guest`), built on demand.

use std::collections::HashMap;
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX, EXE_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Dylib,
    Process,
    /// Needs `rustup target add wasm32-unknown-unknown`.
    Wasm,
    /// The guest speaking protocol version 1 by hand.
    V1,
}

/// Builds the fixture once per version and kind; returns a link to the
/// artifact, so that later builds (which replace it) do not change it.
///
/// Each version has a target directory of its own: on Windows the linker
/// rewrites an existing output in place, so a shared directory would let a
/// v2 build change the file a v1 link points to, or fail to open it while a
/// test uses it.
pub fn artifact(version: &str, kind: Kind) -> PathBuf {
    static BUILT: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
    // a failed build fails its own test, not every later one
    let mut built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    let key = format!("{version}-{kind:?}");
    if let Some(path) = built.get_or_insert_default().get(&key) {
        return path.clone();
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-guest/Cargo.toml");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("rust-guest-{version}"));
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "--quiet", "--manifest-path"]).arg(&manifest).arg("--target-dir").arg(&dir);
    match kind {
        Kind::Dylib => build.arg("--lib"),
        Kind::Process => build.args(["--bin", "rust-guest-process"]),
        Kind::V1 => build.args(["--bin", "rust-guest-v1"]),
        Kind::Wasm => build.args(["--lib", "--target", "wasm32-unknown-unknown"]),
    };
    if version == "v2" {
        build.args(["--features", "v2"]);
    }
    assert!(build.status().unwrap().success(), "cannot build the rust guest for {key}");
    let output = match kind {
        Kind::Dylib => dir.join(format!("debug/{DLL_PREFIX}rust_guest{DLL_SUFFIX}")),
        Kind::Process => dir.join(format!("debug/rust-guest-process{EXE_SUFFIX}")),
        Kind::V1 => dir.join(format!("debug/rust-guest-v1{EXE_SUFFIX}")),
        Kind::Wasm => dir.join("wasm32-unknown-unknown/debug/rust_guest.wasm"),
    };
    let copy = dir.join(format!("{key}-{}", output.file_name().unwrap().to_string_lossy()));
    link(&output, &copy);
    built.get_or_insert_default().insert(key, copy.clone());
    copy
}

/// Executables are linked, never written: a file this process holds open
/// for writing cannot be started while another thread forks ("Text file
/// busy"), as the fork inherits the handle until it execs.
fn link(from: &Path, to: &Path) {
    let _ = std::fs::remove_file(to);
    std::fs::hard_link(from, to).unwrap();
}

/// Puts the artifact at `dir/name`, replacing it through a rename, as an
/// executable that is running cannot be written over.
pub fn install(dir: &Path, name: &str, version: &str, kind: Kind) -> PathBuf {
    let (path, staged) = (dir.join(name), dir.join(format!(".{name}.new")));
    link(&artifact(version, kind), &staged);
    std::fs::rename(&staged, &path).unwrap();
    path
}
