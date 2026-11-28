//! The behavior every file-system provider must share.
//!
//! A provider is checked by mounting it in a `Fs` and running [`Suite::run`].
//! Each case states one rule of the `kardia-fs` contract, so a failure names the
//! rule. A provider that does not declare the capability a case needs reports
//! `NotSupported`, which the suite records as `Skipped` rather than `Failed`.

mod case;
mod suite;

pub use case::Outcome;
pub use suite::Suite;
