use std::collections::BTreeMap;

/// An insertion-ordered registry addressed by never-reused ids.
///
/// Insertion returns a fresh id and removal is by id, so two registrations
/// never interfere with each other and their inverses commute (the paper's
/// commutative key, §3.4.2). Service authors should build their registries
/// (routes, listeners, commands) on it.
#[derive(Debug)]
pub struct IdTable<T> {
    next: u64,
    entries: BTreeMap<u64, T>,
}

impl<T> Default for IdTable<T> {
    fn default() -> Self {
        Self {
            next: 0,
            entries: BTreeMap::new(),
        }
    }
}

impl<T> IdTable<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, value: T) -> u64 {
        self.next += 1;
        self.entries.insert(self.next, value);
        self.next
    }

    pub fn remove(&mut self, id: u64) -> Option<T> {
        self.entries.remove(&id)
    }

    pub fn get(&self, id: u64) -> Option<&T> {
        self.entries.get(&id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Removes every entry, returning them newest first (LIFO).
    pub fn drain_rev(&mut self) -> Vec<T> {
        let mut values: Vec<T> = std::mem::take(&mut self.entries).into_values().collect();
        values.reverse();
        values
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &T)> {
        self.entries.iter().map(|(id, value)| (*id, value))
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_never_reused_and_drain_is_lifo() {
        let mut table = IdTable::new();
        let a = table.insert("a");
        let b = table.insert("b");
        assert_eq!(table.remove(a), Some("a"));
        let c = table.insert("c");
        assert!(c > b);
        assert_eq!(table.remove(a), None);
        assert_eq!(table.drain_rev(), vec!["c", "b"]);
        assert!(table.is_empty());
    }
}
