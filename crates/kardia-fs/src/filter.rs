//! Watch filters: gitignore-like patterns where the last pattern matching an
//! entry or one of its ancestors decides (port of `fs-node/src/watcher.ts`).

use std::sync::Arc;

use kardia_core::{Error, Result};
use globset::{GlobBuilder, GlobMatcher};

/// Decides whether an entry (path relative to the watched directory,
/// `/`-separated) is kept.
#[derive(Clone)]
pub enum Filter {
    /// Patterns; a leading `!` excludes.
    Patterns(Vec<String>),
    Predicate(SharedPredicate),
}

pub type SharedPredicate = Arc<dyn Fn(&str, bool) -> bool + Send + Sync>;

pub type Predicate = Box<dyn Fn(&str, bool) -> bool + Send + Sync>;

impl Filter {
    pub(crate) fn predicate(self) -> Result<Predicate> {
        match self {
            Filter::Predicate(predicate) => Ok(Box::new(move |path, is_dir| predicate(path, is_dir))),
            Filter::Patterns(patterns) => compile(&patterns),
        }
    }
}

struct Rule {
    negated: bool,
    matcher: Option<GlobMatcher>,
    /// The directory before the first glob segment, empty for the watched directory.
    base: String,
    /// Whether the ancestors of an entry are tested.
    upward: bool,
}

impl Rule {
    fn matches(&self, path: &str) -> bool {
        self.matcher.as_ref().is_none_or(|matcher| matcher.is_match(path))
    }

    fn matches_upward(&self, path: &str) -> bool {
        let mut end = path.len();
        while end > 0 {
            if self.matches(&path[..end]) {
                return true;
            }
            end = path[..end].rfind('/').unwrap_or(0);
        }
        false
    }
}

/// Compiles patterns into a predicate.
pub fn compile(patterns: &[String]) -> Result<Predicate> {
    let mut rules = patterns.iter().map(|entry| rule(entry)).collect::<Result<Vec<_>>>()?;
    // Below an entry excluded with no including pattern after it, no entry
    // is reached, so the ancestors need not be tested.
    for rule in rules.iter_mut().rev() {
        if !rule.negated {
            break;
        }
        rule.upward = false;
    }
    Ok(Box::new(move |path, is_dir| {
        let decided =
            rules.iter().rposition(|rule| if rule.upward { rule.matches_upward(path) } else { rule.matches(path) });
        if let Some(index) = decided
            && !rules[index].negated
        {
            return true;
        }
        // a directory is kept if a later including pattern may lead below it
        let after = decided.map_or(0, |index| index + 1);
        is_dir
            && rules[after..]
                .iter()
                .any(|rule| !rule.negated && (contains(&rule.base, path) || contains(path, &rule.base)))
    }))
}

fn rule(entry: &str) -> Result<Rule> {
    let negated = entry.starts_with('!');
    let raw = if negated { &entry[1..] } else { entry };
    let pattern = normalize(raw);
    if raw.starts_with('/') || pattern.split('/').any(|segment| segment == "..") {
        return Err(Error::msg(format!("filter entry {entry:?} leads outside the watched directory")));
    }
    let base: Vec<&str> = pattern.split('/').take_while(|segment| !segment.contains(['*', '?', '[', '{'])).collect();
    let base = if base.len() == pattern.split('/').count() {
        // a literal path: its base is its parent directory
        base[..base.len().saturating_sub(1)].join("/")
    } else {
        base.join("/")
    };
    let matcher = match pattern.as_str() {
        // `.` is the watched directory, which every path below it lies in
        "." => None,
        // patterns are `/`-separated everywhere, so `\` escapes on Windows too
        _ => Some(GlobBuilder::new(&pattern).literal_separator(true).backslash_escape(true).build()?.compile_matcher()),
    };
    Ok(Rule { negated, matcher, base, upward: true })
}

/// Whether an entry is reached: it and every directory above it are kept
/// (a walker never descends into an excluded directory).
pub fn admits(keep: &Predicate, path: &str, is_dir: bool) -> bool {
    let mut end = 0;
    while let Some(offset) = path[end..].find('/') {
        end += offset;
        if !keep(&path[..end], true) {
            return false;
        }
        end += 1;
    }
    keep(path, is_dir)
}

/// Collapses `./`, `//` and a trailing `/`.
fn normalize(pattern: &str) -> String {
    let segments: Vec<&str> = pattern.split('/').filter(|segment| !segment.is_empty() && *segment != ".").collect();
    if segments.is_empty() { ".".into() } else { segments.join("/") }
}

/// Whether `path` is `base` or lies below it.
fn contains(base: &str, path: &str) -> bool {
    base.is_empty() || path == base || path.strip_prefix(base).is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keep(patterns: &[&str]) -> Predicate {
        compile(&patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn the_last_matching_pattern_decides() {
        let filter = keep(&["src/**", "!src/**/*.test.ts", "src/keep.test.ts"]);
        assert!(filter("src/index.ts", false));
        assert!(!filter("src/a/b.test.ts", false));
        assert!(filter("src/keep.test.ts", false));
        assert!(!filter("lib/index.ts", false));
    }

    #[test]
    fn ancestors_decide_for_descendants() {
        let filter = keep(&["dist", "!dist/cache"]);
        assert!(admits(&filter, "dist/a.js", false));
        assert!(!admits(&filter, "dist/cache/x", false));
        assert!(!admits(&filter, "dist/cache", true));
        let filter = keep(&["dist", "!dist/cache", "dist/cache/keep"]);
        assert!(admits(&filter, "dist/cache/keep", false));
        assert!(!admits(&filter, "dist/cache/other", false));
    }

    #[test]
    fn directories_leading_to_included_entries_are_kept() {
        let filter = keep(&["src/lib/*.rs"]);
        assert!(filter("src", true));
        assert!(filter("src/lib", true));
        assert!(!filter("docs", true));
        assert!(filter("src/lib/a.rs", false));
        assert!(!filter("src/main.rs", false));
    }

    #[test]
    fn selects_the_files_by_glob_patterns_relative_to_the_watched_directory() {
        let filter = keep(&["lib", "src/**/*.ts", "packages/*", "!**/.*", "!src/log[!0-9].ts"]);
        for path in ["lib/file.txt", "src/index.ts", "src/deep/index.ts", "src/log1.ts", "packages/a/deep/file.js"] {
            assert!(admits(&filter, path, false), "{path} is selected");
        }
        for path in ["lib/.hidden.txt", "src/index.js", "src/logx.ts", "other.txt"] {
            assert!(!admits(&filter, path, false), "{path} is not selected");
        }
    }

    #[test]
    fn descends_into_a_directory_whose_name_a_pattern_escapes() {
        let filter = keep(&["pkg\\[one\\]/*.ts"]);
        assert!(filter("pkg[one]", true));
        assert!(admits(&filter, "pkg[one]/file.ts", false));
        assert!(!admits(&filter, "pkgo/file.ts", false));
    }

    #[test]
    fn keeps_no_entry_with_an_empty_filter() {
        let filter = keep(&[]);
        assert!(!filter("dir", true));
        assert!(!admits(&filter, "file.txt", false));
    }

    #[test]
    fn the_watched_directory_and_escapes() {
        let filter = keep(&[".", "!*.log"]);
        assert!(filter("a/b.txt", false));
        assert!(!filter("x.log", false));
        assert!(compile(&["../outside".to_string()]).is_err());
        assert!(compile(&["/abs".to_string()]).is_err());
    }
}
