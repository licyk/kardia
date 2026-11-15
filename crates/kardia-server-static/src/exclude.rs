use kardia_core::ValidationError;
use picomatch_rs::{CompileOptions, Matcher, compile_matcher};

pub(super) fn compile(pattern: &str) -> Result<Matcher, ValidationError> {
    // Picomatch's unspecified `posix` enables named classes but leaves a leading
    // `!` literal. picomatch-rs combines these two options into one boolean.
    let mut normalized = String::new();
    let mut chars = pattern.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        normalized.push(c);
        match c {
            '\\' => {
                if let Some(c) = chars.next() {
                    normalized.push(c);
                }
            }
            '"' => quoted = !quoted,
            '[' if !quoted && chars.peek() == Some(&'!') => normalized.push('\\'),
            _ => {}
        }
    }
    let options = CompileOptions { dot: true, windows: cfg!(windows), ..Default::default() };
    compile_matcher(&normalized, &options).map_err(|e| ValidationError::message(format!("exclude: {e:?}")))
}
