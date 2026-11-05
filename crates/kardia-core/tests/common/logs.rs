//! Capturing logged messages.

use std::sync::Arc;

use kardia_core::{Context, Exporter, Level, Message};

use super::Seq;

struct Capture {
    level: Level,
    seq: Seq<String>,
}

impl Exporter for Capture {
    fn export(&self, message: &Message) {
        if message.level == self.level {
            self.seq.push(message.text.clone());
        }
    }

    fn level(&self, _name: &str) -> Option<Level> {
        Some(Level::Debug)
    }
}

/// Records the text of every message logged at `level`.
pub fn capture(ctx: &Context, level: Level) -> Seq<String> {
    let seq = Seq::new();
    ctx.add_exporter(Arc::new(Capture { level, seq: seq.clone() })).unwrap();
    seq
}

pub fn errors(ctx: &Context) -> Seq<String> {
    capture(ctx, Level::Error)
}
