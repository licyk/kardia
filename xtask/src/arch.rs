//! `cargo xtask arch`: entry files (`lib.rs`, `mod.rs`) only declare and
//! re-export modules; implementation lives in files of its own.

use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::{Attribute, Item};

/// An item an entry file must not hold.
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub line: usize,
    pub kind: &'static str,
    pub name: String,
}

pub fn run(root: &Path) -> Result<(), String> {
    let mut found = 0;
    for file in entry_files(root) {
        let source = std::fs::read_to_string(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        let relative = file.strip_prefix(root).unwrap_or(&file).display().to_string();
        let violations = check(&source).map_err(|error| format!("{relative}: {error}"))?;
        for violation in &violations {
            println!("{relative}:{}: {} `{}` in an entry file", violation.line, violation.kind, violation.name);
        }
        found += violations.len();
    }
    if found == 0 {
        println!("Entry files only declare and re-export modules.");
        return Ok(());
    }
    Err(format!(
        "{found} item(s) in lib.rs / mod.rs files: move each into a module of its own, \
         and re-export it from the entry file"
    ))
}

/// The violations in one entry file's source.
pub fn check(source: &str) -> syn::Result<Vec<Violation>> {
    let file = syn::parse_file(source)?;
    let mut violations = Vec::new();
    check_items(&file.items, &mut violations);
    Ok(violations)
}

fn check_items(items: &[Item], violations: &mut Vec<Violation>) {
    for item in items {
        let (kind, name, line) = match item {
            Item::Use(_) | Item::ExternCrate(_) => continue,
            Item::Mod(module) => {
                // an inline module follows the same rule: it may only re-export
                if let Some((_, items)) = &module.content {
                    check_items(items, violations);
                }
                continue;
            }
            // Rust requires proc-macro entry points at the crate root
            Item::Fn(function) if is_proc_macro(&function.attrs) => continue,
            Item::Fn(function) => named("fn", &function.sig.ident),
            Item::Struct(item) => named("struct", &item.ident),
            Item::Enum(item) => named("enum", &item.ident),
            Item::Union(item) => named("union", &item.ident),
            Item::Trait(item) => named("trait", &item.ident),
            Item::TraitAlias(item) => named("trait alias", &item.ident),
            Item::Type(item) => named("type", &item.ident),
            Item::Const(item) => named("const", &item.ident),
            Item::Static(item) => named("static", &item.ident),
            Item::Impl(item) => ("impl", impl_name(item), item.impl_token.span.start().line),
            Item::Macro(item) => match &item.ident {
                Some(ident) => named("macro_rules!", ident),
                None => ("macro call", path_name(&item.mac.path), item.mac.path.span().start().line),
            },
            Item::ForeignMod(item) => ("extern block", String::new(), item.abi.span().start().line),
            _ => ("item", String::new(), item.span().start().line),
        };
        violations.push(Violation { line, kind, name });
    }
}

/// A named item, at the line of its name rather than of its attributes.
fn named(kind: &'static str, ident: &syn::Ident) -> (&'static str, String, usize) {
    (kind, ident.to_string(), ident.span().start().line)
}

fn is_proc_macro(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        path.is_ident("proc_macro") || path.is_ident("proc_macro_derive") || path.is_ident("proc_macro_attribute")
    })
}

fn impl_name(item: &syn::ItemImpl) -> String {
    let target = type_name(&item.self_ty);
    match &item.trait_ {
        Some((_, path, _)) => format!("{} for {target}", path_name(path)),
        None => target,
    }
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path_name(&path.path),
        _ => "_".into(),
    }
}

fn path_name(path: &syn::Path) -> String {
    path.segments.last().map(|segment| segment.ident.to_string()).unwrap_or_default()
}

/// Every `lib.rs` and `mod.rs` of the workspace's crates and of xtask.
/// Test fixtures that are crates of their own are not part of the workspace.
fn entry_files(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.join("xtask/src")];
    if let Ok(crates) = std::fs::read_dir(root.join("crates")) {
        for entry in crates.flatten() {
            dirs.push(entry.path().join("src"));
            dirs.push(entry.path().join("tests"));
        }
    }
    let mut files = Vec::new();
    for dir in dirs {
        collect(&dir, &mut files);
    }
    files.sort();
    files
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "fixtures") {
                collect(&path, files);
            }
        } else if path.file_name().is_some_and(|name| name == "lib.rs" || name == "mod.rs") {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(usize, &'static str, String)> {
        check(source).unwrap().into_iter().map(|v| (v.line, v.kind, v.name)).collect()
    }

    #[test]
    fn allows_declarations_reexports_and_docs() {
        let source = r#"
//! Crate docs.
#![forbid(unsafe_code)]

mod private;
pub mod public;
#[cfg(feature = "x")]
mod optional;

pub use public::{Thing, Other};
use std::sync::Arc;
extern crate alloc;

/// Re-exports only.
pub mod prelude {
    pub use crate::public::Thing;
}
"#;
        assert_eq!(kinds(source), vec![]);
    }

    #[test]
    fn allows_proc_macro_entry_points() {
        let source = r#"
#[proc_macro_derive(Service, attributes(service))]
pub fn derive_service(input: TokenStream) -> TokenStream { service::expand(input) }
#[proc_macro_attribute]
pub fn plugin(args: TokenStream, item: TokenStream) -> TokenStream { plugin::expand(args, item) }
"#;
        assert_eq!(kinds(source), vec![]);
    }

    #[test]
    fn reports_every_kind_of_implementation_with_its_line() {
        let source = r#"mod ok;
pub fn helper() {}
pub struct Config;
enum State { A }
pub trait Ext {}
impl Ext for Config {}
impl Config {}
type Alias = u8;
const LIMIT: usize = 3;
static NAME: &str = "x";
macro_rules! shout { () => {} }
union Bits { a: u8 }
"#;
        assert_eq!(
            kinds(source),
            vec![
                (2, "fn", "helper".into()),
                (3, "struct", "Config".into()),
                (4, "enum", "State".into()),
                (5, "trait", "Ext".into()),
                (6, "impl", "Ext for Config".into()),
                (7, "impl", "Config".into()),
                (8, "type", "Alias".into()),
                (9, "const", "LIMIT".into()),
                (10, "static", "NAME".into()),
                (11, "macro_rules!", "shout".into()),
                (12, "union", "Bits".into()),
            ]
        );
    }

    #[test]
    fn checks_inline_modules_including_tests() {
        let source = r#"
pub mod prelude {
    pub use crate::Thing;
    pub fn sneaky() {}
}
#[cfg(test)]
mod tests {
    #[test]
    fn works() {}
}
"#;
        assert_eq!(kinds(source), vec![(4, "fn", "sneaky".into()), (9, "fn", "works".into())]);
    }

    #[test]
    fn finds_entry_files_but_not_fixture_crates() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let files = entry_files(root);
        assert!(files.iter().any(|file| file.ends_with("crates/kardia-core/src/lib.rs")));
        assert!(files.iter().any(|file| file.ends_with("crates/kardia-core/tests/common/mod.rs")));
        assert!(!files.iter().any(|file| file.components().any(|part| part.as_os_str() == "fixtures")));
        assert!(!files.iter().any(|file| file.ends_with("xtask/src/main.rs")));
    }
}
