//! The version type the release commands share: a plain `major.minor.patch`,
//! the increment levels, and the requirement users write for it.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Patch,
    Minor,
    Major,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// A plain `major.minor.patch`; pre-releases and build metadata are refused.
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Vec<u64> = text.split('.').map(|part| part.parse().ok()).collect::<Option<_>>().unwrap_or_default();
        match parts[..] {
            [major, minor, patch] => Ok(Self { major, minor, patch }),
            _ => Err(format!("`{text}` is not a plain major.minor.patch version")),
        }
    }

    pub fn bump(self, level: Level) -> Self {
        match level {
            Level::Patch => Self { patch: self.patch + 1, ..self },
            Level::Minor => Self { minor: self.minor + 1, patch: 0, ..self },
            Level::Major => Self { major: self.major + 1, minor: 0, patch: 0 },
        }
    }

    /// The requirement a user writes for this version (`kardia = "0.1"`):
    /// before 1.0 a minor release may break, after it only a major one.
    pub fn requirement(self) -> String {
        if self.major == 0 { format!("0.{}", self.minor) } else { self.major.to_string() }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A file and its new contents.
#[derive(Debug)]
pub struct Change {
    pub path: PathBuf,
    pub contents: String,
}

pub fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn bumps_each_level() {
        assert_eq!(version("0.1.3").bump(Level::Patch), version("0.1.4"));
        assert_eq!(version("0.1.3").bump(Level::Minor), version("0.2.0"));
        assert_eq!(version("0.4.2").bump(Level::Major), version("1.0.0"));
        assert_eq!(version("1.2.3").bump(Level::Minor), version("1.3.0"));
    }

    #[test]
    fn refuses_what_is_not_major_minor_patch() {
        for text in ["0.1", "0.1.0-alpha", "1.2.3.4", "x.1.0", ""] {
            assert!(Version::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn requirement_follows_semver_compatibility() {
        assert_eq!(version("0.1.9").requirement(), "0.1");
        assert_eq!(version("1.4.0").requirement(), "1");
    }
}
