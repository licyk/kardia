use std::sync::LazyLock;

use kardia_core::{Error, Result};
use regress::Regex;
use serde_json::{Map, Value};

#[derive(Clone)]
pub enum Path {
    String(String),
    Regex(Regex),
}

impl Path {
    pub fn regex(source: &str, flags: &str) -> Result<Self> {
        Ok(Self::Regex(Regex::with_flags(source, flags)?))
    }
}
impl From<&str> for Path {
    fn from(path: &str) -> Self {
        Self::String(path.into())
    }
}
impl From<String> for Path {
    fn from(path: String) -> Self {
        Self::String(path)
    }
}

pub(crate) struct Pattern {
    regex: Regex,
    keys: Option<Vec<String>>,
}

impl Pattern {
    pub fn new(path: &Path) -> Result<Self> {
        let Path::String(path) = path else {
            let Path::Regex(regex) = path else { unreachable!() };
            return Ok(Self { regex: regex.clone(), keys: None });
        };
        let tokens = parse(&mut path.chars().peekable(), false)?;
        let mut keys = Vec::new();
        let sources = flatten(&tokens)?.iter().map(|tokens| source(tokens, &mut keys)).collect::<Result<Vec<_>>>()?;
        let regex = Regex::with_flags(&format!("^(?:{})(?:/$)?$", sources.join("|")), "i")?;
        Ok(Self { regex, keys: Some(keys) })
    }

    pub fn check(&self, path: &str) -> Result<Option<Value>> {
        let Some(capture) = self.regex.find(path) else { return Ok(None) };
        let Some(keys) = &self.keys else {
            return Ok(Some(Value::Array(
                capture.groups().map(|range| range.map_or(Value::Null, |r| Value::String(path[r].into()))).collect(),
            )));
        };
        let mut params = Map::new();
        for (index, name) in keys.iter().enumerate() {
            if let Some(range) = capture.group(index + 1) {
                let raw = path[range].replace('+', " ");
                let bytes = raw.as_bytes();
                for (i, byte) in bytes.iter().enumerate() {
                    if *byte == b'%'
                        && (i + 2 >= bytes.len()
                            || !bytes[i + 1].is_ascii_hexdigit()
                            || !bytes[i + 2].is_ascii_hexdigit())
                    {
                        return Err(Error::msg("invalid URI parameter escape"));
                    }
                }
                params.insert(
                    name.clone(),
                    percent_encoding::percent_decode_str(&raw).decode_utf8()?.into_owned().into(),
                );
            }
        }
        Ok(Some(Value::Object(params)))
    }
}

#[derive(Clone)]
enum Token {
    Text(String),
    Param(String),
    Wildcard(String),
    Group(Vec<Token>),
}

// The token grammar and backtracking exclusions follow path-to-regexp 8.3 (MIT).
fn parse(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, group: bool) -> Result<Vec<Token>> {
    static START: LazyLock<Regex> = LazyLock::new(|| Regex::with_flags(r"^[$_\p{ID_Start}]$", "u").unwrap());
    static CONTINUE: LazyLock<Regex> =
        LazyLock::new(|| Regex::with_flags(r"^[$\u200c\u200d\p{ID_Continue}]$", "u").unwrap());
    let mut tokens = Vec::new();
    let mut text = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '}' if group => {
                if !text.is_empty() {
                    tokens.push(Token::Text(text));
                }
                return Ok(tokens);
            }
            '\\' => text.push(chars.next().ok_or_else(|| Error::msg("unexpected end after escape"))?),
            ':' | '*' | '{' => {
                if !text.is_empty() {
                    tokens.push(Token::Text(std::mem::take(&mut text)));
                }
                if ch == '{' {
                    tokens.push(Token::Group(parse(chars, true)?));
                    continue;
                }
                let mut name = String::new();
                if chars.peek() == Some(&'"') {
                    chars.next();
                    loop {
                        match chars.next().ok_or_else(|| Error::msg("unterminated parameter name"))? {
                            '"' => break,
                            '\\' => name.push(chars.next().ok_or_else(|| Error::msg("unterminated parameter escape"))?),
                            c => name.push(c),
                        }
                    }
                } else if chars.peek().is_some_and(|c| START.find(&c.to_string()).is_some()) {
                    name.push(chars.next().unwrap());
                    while chars.peek().is_some_and(|c| CONTINUE.find(&c.to_string()).is_some()) {
                        name.push(chars.next().unwrap());
                    }
                }
                if name.is_empty() {
                    return Err(Error::msg("missing parameter name"));
                }
                tokens.push(if ch == ':' { Token::Param(name) } else { Token::Wildcard(name) });
            }
            '}' | '(' | ')' | '[' | ']' | '+' | '?' | '!' => {
                return Err(Error::msg(format!("unexpected path token {ch}")));
            }
            c => text.push(c),
        }
    }
    if group {
        return Err(Error::msg("unclosed path group"));
    }
    if !text.is_empty() {
        tokens.push(Token::Text(text));
    }
    Ok(tokens)
}

fn flatten(tokens: &[Token]) -> Result<Vec<Vec<Token>>> {
    let mut sequences = vec![Vec::new()];
    for token in tokens {
        match token {
            Token::Group(group) => {
                let alternatives = flatten(group)?;
                sequences = sequences
                    .into_iter()
                    .flat_map(|prefix| {
                        alternatives
                            .iter()
                            .map(|suffix| prefix.iter().chain(suffix).cloned().collect())
                            .chain(std::iter::once(prefix.clone()))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                if sequences.len() > 256 {
                    return Err(Error::msg("too many optional path combinations"));
                }
            }
            _ => {
                for sequence in &mut sequences {
                    sequence.push(token.clone());
                }
            }
        }
    }
    Ok(sequences)
}

fn escape(value: &str) -> String {
    let mut result = String::new();
    for ch in value.chars() {
        if ".+*?^${}()[]|/\\".contains(ch) {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

fn negate(a: &str, b: &str) -> String {
    if b.len() > a.len() {
        return negate(b, a);
    }
    let b = if a == b { "" } else { b };
    if b.chars().count() > 1 {
        format!("(?:(?!{}|{})[^])", escape(a), escape(b))
    } else if a.chars().count() > 1 {
        format!("(?:(?!{})[^{}])", escape(a), escape(b))
    } else {
        format!("[^{}{}]", escape(a), escape(b))
    }
}

fn source(tokens: &[Token], keys: &mut Vec<String>) -> Result<String> {
    let mut result = String::new();
    let mut backtrack = String::new();
    let mut wildcard_backtrack = String::new();
    let mut previous = 0;
    let mut segment = 0;
    for (index, token) in tokens.iter().enumerate() {
        if let Token::Text(text) = token {
            result.push_str(&escape(text));
            backtrack.push_str(text);
            if previous == 2 {
                wildcard_backtrack.push_str(text);
            }
            if text.contains('/') {
                segment = 0;
            }
            continue;
        }
        let (Token::Param(name) | Token::Wildcard(name)) = token else { unreachable!() };
        if previous != 0 && backtrack.is_empty() {
            return Err(Error::msg(format!("missing text before parameter {name}")));
        }
        let capture = match token {
            Token::Param(_) => {
                let later_wildcard = tokens[index + 1..]
                    .iter()
                    .take_while(|t| !matches!(t, Token::Text(s) if s.contains('/')))
                    .any(|t| matches!(t, Token::Wildcard(_)));
                let text: String = tokens[index + 1..]
                    .iter()
                    .map_while(|t| if let Token::Text(s) = t { Some(s.as_str()) } else { None })
                    .collect();
                let capture = if segment & 2 != 0 {
                    format!("({}+)", negate("/", &backtrack))
                } else if later_wildcard {
                    format!("({}+)", negate("/", &text))
                } else if segment & 1 != 0 {
                    format!("({}+|{})", negate("/", &backtrack), escape(&backtrack))
                } else {
                    format!("({}+)", negate("/", ""))
                };
                previous = 1;
                capture
            }
            Token::Wildcard(_) => {
                let capture = if segment & 2 != 0 {
                    format!("({}+)", negate(&backtrack, ""))
                } else if !wildcard_backtrack.is_empty() {
                    format!("({}+|{}+)", negate(&wildcard_backtrack, ""), negate("/", ""))
                } else {
                    "([^]+)".into()
                };
                wildcard_backtrack.clear();
                previous = 2;
                capture
            }
            _ => unreachable!(),
        };
        result.push_str(&capture);
        segment |= previous;
        keys.push(name.clone());
        backtrack.clear();
    }
    Ok(result)
}
