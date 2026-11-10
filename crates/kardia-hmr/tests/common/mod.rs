// each test crate uses a part of these
#![allow(dead_code, unused_imports)]

mod app;
mod fixture;
mod scratch;

pub use app::{App, fiber};
pub use fixture::{Kind, artifact, install};
pub use scratch::scratch;
