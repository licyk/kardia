//! YAML (core schema) to JSON values, with node positions, and back.
//!
//! `!!js <source>` reads as an expression object `{ "__jsExpr": source }`
//! and is written back the same way. Unknown tags are errors, since they
//! would leave the data incomplete.

use std::borrow::Cow;
use std::collections::HashMap;

use cordis_loader::{EXPR_KEY, expr_source};
use saphyr_parser::{Event, Marker, Parser, ScalarStyle, Tag};
use serde_json::{Map, Number, Value};

/// A position in the source (1-based line and column, in characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl From<Marker> for Position {
    fn from(marker: Marker) -> Self {
        Self { line: marker.line(), column: marker.col() + 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ParseError {
    pub message: String,
    pub position: Position,
}

/// Node positions of a parsed document, for locating values by path.
#[derive(Debug, Clone)]
pub struct Syntax {
    root: Option<Node>,
}

#[derive(Debug, Clone)]
struct Node {
    start: Position,
    kind: NodeKind,
}

#[derive(Debug, Clone)]
enum NodeKind {
    Scalar,
    Seq(Vec<Node>),
    /// Entries with the position of their key.
    Map(Vec<(String, Position, Node)>),
}

/// A step into a document: a map key or a sequence index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Index(usize),
}

impl Syntax {
    /// Where the value at `path` is written: a scalar at itself, a
    /// collection at its key. A path leaving the document falls back to
    /// its deepest written ancestor.
    pub fn locate(&self, path: &[Step]) -> Option<Position> {
        let mut node = self.root.as_ref()?;
        let mut anchor = node.start;
        for step in path {
            let next = match (&node.kind, step) {
                (NodeKind::Map(entries), Step::Key(key)) => {
                    entries.iter().find(|(name, ..)| name == key).map(|(_, key, value)| (*key, value))
                }
                (NodeKind::Seq(items), Step::Index(index)) => items.get(*index).map(|item| (item.start, item)),
                _ => None,
            };
            let Some((position, value)) = next else { break };
            anchor = position;
            node = value;
        }
        if matches!(node.kind, NodeKind::Scalar) {
            anchor = node.start;
        }
        Some(anchor)
    }
}

enum Frame {
    Seq {
        anchor: usize,
        start: Position,
        values: Vec<Value>,
        nodes: Vec<Node>,
    },
    Map {
        anchor: usize,
        start: Position,
        values: Map<String, Value>,
        nodes: Vec<(String, Position, Node)>,
        key: Option<(String, Position)>,
    },
}

/// Parses the first document of `content`.
pub fn parse(content: &str) -> Result<(Value, Syntax), ParseError> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut anchors: HashMap<usize, (Value, Node)> = HashMap::new();
    let mut result: Option<(Value, Node)> = None;
    for event in Parser::new_from_str(content) {
        let (event, span) = event
            .map_err(|error| ParseError { message: error.info().to_string(), position: (*error.marker()).into() })?;
        let start = Position::from(span.start);
        let completed = match event {
            Event::StreamStart | Event::DocumentStart(_) | Event::Nothing => None,
            Event::DocumentEnd | Event::StreamEnd => break,
            Event::Alias(id) => {
                let (value, node) = anchors
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| ParseError { message: "unknown alias".into(), position: start })?;
                Some((value, Node { start, kind: node.kind }, 0))
            }
            Event::Scalar(value, style, anchor, tag) => {
                let value =
                    scalar(&value, style, tag.as_deref()).map_err(|message| ParseError { message, position: start })?;
                Some((value, Node { start, kind: NodeKind::Scalar }, anchor))
            }
            Event::SequenceStart(anchor, tag) => {
                check_collection_tag(tag.as_deref(), "seq", start)?;
                stack.push(Frame::Seq { anchor, start, values: Vec::new(), nodes: Vec::new() });
                None
            }
            Event::MappingStart(anchor, tag) => {
                check_collection_tag(tag.as_deref(), "map", start)?;
                stack.push(Frame::Map { anchor, start, values: Map::new(), nodes: Vec::new(), key: None });
                None
            }
            Event::SequenceEnd | Event::MappingEnd => match stack.pop() {
                Some(Frame::Seq { anchor, start, values, nodes }) => {
                    Some((Value::Array(values), Node { start, kind: NodeKind::Seq(nodes) }, anchor))
                }
                Some(Frame::Map { anchor, start, values, nodes, .. }) => {
                    Some((Value::Object(values), Node { start, kind: NodeKind::Map(nodes) }, anchor))
                }
                None => None,
            },
        };
        let Some((value, node, anchor)) = completed else { continue };
        if anchor > 0 {
            anchors.insert(anchor, (value.clone(), node.clone()));
        }
        match stack.last_mut() {
            None => {
                result.get_or_insert((value, node));
            }
            Some(Frame::Seq { values, nodes, .. }) => {
                values.push(value);
                nodes.push(node);
            }
            Some(Frame::Map { values, nodes, key, .. }) => match key.take() {
                None => {
                    let name = match value {
                        Value::String(name) => name,
                        Value::Null => "null".into(),
                        Value::Bool(_) | Value::Number(_) => value.to_string(),
                        _ => {
                            return Err(ParseError {
                                message: "complex keys are not supported".into(),
                                position: node.start,
                            });
                        }
                    };
                    *key = Some((name, node.start));
                }
                Some((name, position)) => {
                    values.insert(name.clone(), value);
                    nodes.push((name, position, node));
                }
            },
        }
    }
    let (value, node) =
        result.unwrap_or((Value::Null, Node { start: Position { line: 1, column: 1 }, kind: NodeKind::Scalar }));
    Ok((value, Syntax { root: Some(node) }))
}

const CORE: &str = "tag:yaml.org,2002:";

fn check_collection_tag(tag: Option<&Tag>, expected: &str, position: Position) -> Result<(), ParseError> {
    match tag {
        None => Ok(()),
        Some(tag) if tag.handle == CORE && tag.suffix == expected => Ok(()),
        Some(tag) => Err(ParseError { message: format!("unresolved tag: {}{}", tag.handle, tag.suffix), position }),
    }
}

fn scalar(value: &str, style: ScalarStyle, tag: Option<&Tag>) -> Result<Value, String> {
    if let Some(tag) = tag {
        if tag.handle != CORE {
            return Err(format!("unresolved tag: {}{}", tag.handle, tag.suffix));
        }
        return match tag.suffix.as_str() {
            "js" => Ok(Value::Object(Map::from_iter([(EXPR_KEY.to_string(), Value::String(value.to_string()))]))),
            "str" => Ok(Value::String(value.to_string())),
            "null" | "bool" | "int" | "float" => resolve_plain(value)
                .filter(|v| !v.is_string())
                .ok_or_else(|| format!("invalid !!{} value: {value}", tag.suffix)),
            suffix => Err(format!("unresolved tag: !!{suffix}")),
        };
    }
    if style != ScalarStyle::Plain {
        return Ok(Value::String(value.to_string()));
    }
    Ok(resolve_plain(value).unwrap_or_else(|| Value::String(value.to_string())))
}

/// Core schema resolution of a plain scalar (`None` for a plain string).
fn resolve_plain(value: &str) -> Option<Value> {
    match value {
        "" | "~" | "null" | "Null" | "NULL" => return Some(Value::Null),
        "true" | "True" | "TRUE" => return Some(Value::Bool(true)),
        "false" | "False" | "FALSE" => return Some(Value::Bool(false)),
        _ => {}
    }
    let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
    if digits(unsigned, 10) {
        return value.parse::<i64>().ok().map(Value::from).or_else(|| value.parse::<f64>().ok().and_then(float));
    }
    if let Some(octal) = value.strip_prefix("0o").filter(|s| digits(s, 8)) {
        return i64::from_str_radix(octal, 8).ok().map(Value::from);
    }
    if let Some(hex) = value.strip_prefix("0x").filter(|s| digits(s, 16)) {
        return i64::from_str_radix(hex, 16).ok().map(Value::from);
    }
    if is_float(unsigned) {
        return value.parse::<f64>().ok().and_then(float);
    }
    None
}

/// `(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?` (special floats stay strings, as JSON has none).
fn is_float(value: &str) -> bool {
    let (mantissa, exponent) = match value.find(['e', 'E']) {
        Some(index) => (&value[..index], Some(&value[index + 1..])),
        None => (value, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (mantissa, None),
    };
    let all_digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    let mantissa_ok = match frac {
        Some(frac) => all_digits(int) && all_digits(frac) && !(int.is_empty() && frac.is_empty()),
        None => !int.is_empty() && all_digits(int),
    };
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && all_digits(e)
    });
    mantissa_ok && exponent_ok && (frac.is_some() || exponent.is_some())
}

fn float(value: f64) -> Option<Value> {
    Number::from_f64(value).map(Value::Number)
}

/// Dumps entry data as YAML (block style, single-quoted strings where needed).
pub fn dump(value: &Value) -> String {
    let mut output = String::new();
    write_block(&mut output, value, 0);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn write_block(output: &mut String, value: &Value, indent: usize) {
    match value {
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                output.push_str(&" ".repeat(indent));
                output.push('-');
                match item {
                    Value::Object(map) if !map.is_empty() && expr_source(item).is_none() => {
                        output.push(' ');
                        write_map(output, map, indent + 2, true);
                    }
                    Value::Array(inner) if !inner.is_empty() => {
                        output.push('\n');
                        write_block(output, item, indent + 2);
                    }
                    _ => {
                        output.push(' ');
                        output.push_str(&inline(item));
                        output.push('\n');
                    }
                }
            }
        }
        Value::Object(map) if !map.is_empty() && expr_source(value).is_none() => write_map(output, map, indent, false),
        _ => {
            output.push_str(&" ".repeat(indent));
            output.push_str(&inline(value));
            output.push('\n');
        }
    }
}

fn write_map(output: &mut String, map: &Map<String, Value>, indent: usize, continued: bool) {
    for (index, (key, value)) in map.iter().enumerate() {
        if index > 0 || !continued {
            output.push_str(&" ".repeat(indent));
        }
        output.push_str(&string(key));
        output.push(':');
        match value {
            Value::Object(inner) if !inner.is_empty() && expr_source(value).is_none() => {
                output.push('\n');
                write_map(output, inner, indent + 2, false);
            }
            Value::Array(items) if !items.is_empty() => {
                output.push('\n');
                write_block(output, value, indent);
            }
            _ => {
                output.push(' ');
                output.push_str(&inline(value));
                output.push('\n');
            }
        }
    }
}

fn inline(value: &Value) -> String {
    if let Some(source) = expr_source(value) {
        return format!("!!js {}", quoted(source));
    }
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => string(text),
        Value::Array(_) => "[]".into(),
        Value::Object(_) => "{}".into(),
    }
}

/// A string scalar: plain when it reads back as the same string.
fn string(text: &str) -> String {
    if is_plain_safe(text) { text.to_string() } else { quoted(text) }
}

fn quoted(text: &str) -> String {
    if text.chars().any(|c| c.is_control()) {
        return serde_json::to_string(text).expect("strings serialize");
    }
    format!("'{}'", text.replace('\'', "''"))
}

fn is_plain_safe(text: &str) -> bool {
    let Some(first) = text.chars().next() else { return false };
    !"-?:,[]{}#&*!|>'\"%@`".contains(first)
        && !text.starts_with(char::is_whitespace)
        && !text.ends_with(char::is_whitespace)
        && !text.contains(": ")
        && !text.contains(" #")
        && !text.ends_with(':')
        && !text.chars().any(|c| c.is_control())
        && resolve_plain(text).is_none()
        && !matches!(Cow::from(text).as_ref(), ".inf" | ".nan" | "-.inf" | ".Inf" | ".NaN")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_the_core_schema_with_expressions_and_aliases() {
        let source = "- id: a\n  name: foo\n  disabled: !!js 'x > 1'\n  config: &c { a: 1, b: [1, 2.5, ~, true, 'q', 0x1f] }\n- *c\n- !!str 12\n";
        let (value, syntax) = parse(source).unwrap();
        assert_eq!(
            value,
            json!([
                { "id": "a", "name": "foo", "disabled": { "__jsExpr": "x > 1" }, "config": { "a": 1, "b": [1, 2.5, null, true, "q", 31] } },
                { "a": 1, "b": [1, 2.5, null, true, "q", 31] },
                "12",
            ])
        );
        let at = |path: Vec<Step>| syntax.locate(&path).unwrap();
        assert_eq!(at(vec![Step::Index(0), Step::Key("name".into())]), Position { line: 2, column: 9 });
        assert_eq!(at(vec![Step::Index(0), Step::Key("config".into())]), Position { line: 4, column: 3 });
        assert_eq!(at(vec![Step::Index(0), Step::Key("missing".into())]), Position { line: 1, column: 3 });
    }

    #[test]
    fn reports_errors_and_unknown_tags() {
        let error = parse("a: [1, 2\nb: c").unwrap_err();
        assert_eq!(error.position.line, 2);
        assert!(parse("- !custom x").unwrap_err().message.contains("unresolved tag"));
    }

    #[test]
    fn dumps_values_that_read_back_the_same() {
        let value = json!([
            { "id": "a", "name": "foo", "config": { "text": "it's: here", "n": "12", "empty": "", "list": [1, { "x": null }], "map": {} } },
            { "id": "b", "name": "group", "group": true, "config": [{ "id": "c", "name": "bar", "disabled": { "__jsExpr": "a && b" } }] },
            { "id": "d", "name": "multi", "config": "line\nbreak" },
        ]);
        let text = dump(&value);
        assert_eq!(parse(&text).unwrap().0, value, "{text}");
        assert!(text.starts_with("- id: a\n  name: foo\n  config:\n    text: 'it''s: here'\n    n: '12'\n"), "{text}");
        assert!(text.contains("disabled: !!js 'a && b'"), "{text}");
    }
}
