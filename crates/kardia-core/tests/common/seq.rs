//! A shared, ordered record of observations.

use std::sync::Arc;

use parking_lot::Mutex;

/// A shared, ordered record of observations.
pub struct Seq<T>(Arc<Mutex<Vec<T>>>);

impl<T> Clone for Seq<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for Seq<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<T: Clone> Seq<T> {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    pub fn push(&self, value: T) {
        self.0.lock().push(value);
    }

    pub fn get(&self) -> Vec<T> {
        self.0.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.0.lock().len()
    }

    pub fn clear(&self) {
        self.0.lock().clear();
    }

    /// A closure pushing `value`, for use as a disposer.
    pub fn pusher(&self, value: T) -> impl FnOnce() + Send + 'static
    where
        T: Send + 'static,
    {
        let seq = self.clone();
        move || seq.push(value)
    }
}
