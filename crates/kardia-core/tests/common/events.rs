//! Events defined for tests.

use kardia_core::Event;

/// A custom event for tests (`'custom-event'`).
pub struct Custom;

impl Event for Custom {
    type Output = ();
}
