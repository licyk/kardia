use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Name of a coeffect (service), e.g. `"database"` or `"foo.bar"`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(Arc<str>);

impl Key {
    pub fn new(name: impl AsRef<str>) -> Self {
        Self(Arc::from(name.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for Key {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Key {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Key {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

impl From<String> for Key {
    fn from(name: String) -> Self {
        Self(Arc::from(name))
    }
}

impl From<&String> for Key {
    fn from(name: &String) -> Self {
        Self::new(name)
    }
}

impl From<&Key> for Key {
    fn from(key: &Key) -> Self {
        key.clone()
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

/// A realm (the paper's isolation symbol). Realms are generative: every call
/// to [`RealmId::new`] yields a fresh one, shared only by explicit reuse.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RealmId(u64);

static REALM_COUNTER: AtomicU64 = AtomicU64::new(0);

impl RealmId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(REALM_COUNTER.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

impl fmt::Debug for RealmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "realm#{}", self.0)
    }
}

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub(crate) u64);

        impl $name {
            pub fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "#{}"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// Identity of one provider binding; never reused, so "replaced by an
    /// equal value" is observable.
    ImplId,
    "impl"
);
id_type!(
    /// The paper's fiber name; never reused within a root. `0` is the root.
    FiberId,
    "fiber"
);

impl FiberId {
    pub const ROOT: FiberId = FiberId(0);
}
