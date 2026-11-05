//! Expressions in entry options (`{ "__jsExpr": "..." }`, YAML `!!js`).
//!
//! JavaScript `eval` cannot be ported, so expressions use a small,
//! side-effect-free subset of JavaScript over JSON values: literals, array
//! and object literals, member access (`a.b`, `a[0]`, `.length`), unary
//! `! - +`, arithmetic, comparison, (strict) equality, `&& || ??` and
//! `?:`. A free identifier reads the JSON view of the service of that name
//! through checked access, so the usual `inject` rules apply.

use kardia_core::{Context, Error, PathSegment, Result};
use serde_json::{Map, Number, Value};

use crate::options::expr_source;

/// An expression in entry options that failed.
#[derive(Debug, Clone, thiserror::Error)]
#[error("cannot evaluate `{expr}`: {message}")]
pub struct ExprError {
    /// Where the expression sits in the entry options.
    pub path: Vec<PathSegment>,
    pub expr: String,
    /// The char of `expr` the failure points at (its start for a syntax error).
    pub offset: usize,
    pub message: String,
}

/// A failure inside an expression, with the char it points at.
struct Fault {
    error: Error,
    offset: usize,
}

impl From<Error> for Fault {
    fn from(error: Error) -> Self {
        Self { error, offset: 0 }
    }
}

/// Evaluates an expression in `ctx`.
pub fn evaluate(ctx: &Context, source: &str) -> Result<Value> {
    run(ctx, source).map_err(|fault| fault.error)
}

/// Evaluates the expression at `path` of entry options; fails with an [`ExprError`].
pub(crate) fn evaluate_at(ctx: &Context, source: &str, path: &[PathSegment]) -> Result<Value> {
    run(ctx, source).map_err(|fault| {
        let message = fault.error.to_string();
        ExprError { path: path.to_vec(), expr: source.to_string(), offset: fault.offset, message }.into()
    })
}

fn run(ctx: &Context, source: &str) -> Result<Value, Fault> {
    let (tokens, offsets) = lex(source)?.into_iter().unzip();
    let mut parser = Parser { tokens, offsets, position: 0 };
    let expr = parser.ternary()?;
    if parser.position < parser.tokens.len() {
        return Err(syntax(source, "unexpected token").into());
    }
    eval(ctx, &expr)
}

/// Replaces every expression inside `value` by its value.
pub fn interpolate(ctx: &Context, value: &Value) -> Result<Value> {
    interpolate_at(ctx, value, &mut Vec::new())
}

/// [`interpolate`] for the value at `path` of entry options.
pub(crate) fn interpolate_at(ctx: &Context, value: &Value, path: &mut Vec<PathSegment>) -> Result<Value> {
    if let Some(source) = expr_source(value) {
        return evaluate_at(ctx, source, path);
    }
    Ok(match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    path.push(PathSegment::Index(index));
                    let value = interpolate_at(ctx, item, path);
                    path.pop();
                    value
                })
                .collect::<Result<_>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| {
                    path.push(PathSegment::Key(key.clone()));
                    let value = interpolate_at(ctx, item, path);
                    path.pop();
                    Ok((key.clone(), value?))
                })
                .collect::<Result<_>>()?,
        ),
        other => other.clone(),
    })
}

/// Whether `value` contains an expression anywhere.
pub fn has_expr(value: &Value) -> bool {
    expr_source(value).is_some()
        || match value {
            Value::Array(items) => items.iter().any(has_expr),
            Value::Object(map) => map.values().any(has_expr),
            _ => false,
        }
}

fn syntax(source: &str, message: &str) -> Error {
    Error::msg(format!("syntax error in `{source}`: {message}"))
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Str(String),
    Ident(String),
    Punct(&'static str),
}

const PUNCTS: [&str; 28] = [
    "===", "!==", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "?", ":", ".", ",", "(", ")", "[", "]", "{", "}",
    "<", ">", "!", "+", "-", "*", "/", "%",
];

/// Tokens with the char offset each starts at.
fn lex(source: &str) -> Result<Vec<(Token, usize)>> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let number = text.parse::<f64>().map_err(|_| syntax(source, &format!("invalid number {text}")))?;
            tokens.push((Token::Number(number), start));
        } else if c == '\'' || c == '"' || c == '`' {
            let start = i;
            let mut text = String::new();
            i += 1;
            loop {
                let Some(&next) = chars.get(i) else { return Err(syntax(source, "unterminated string")) };
                i += 1;
                match next {
                    _ if next == c => break,
                    '\\' => {
                        let escaped = chars.get(i).copied().ok_or_else(|| syntax(source, "unterminated string"))?;
                        i += 1;
                        text.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '0' => '\0',
                            other => other,
                        });
                    }
                    other => text.push(other),
                }
            }
            tokens.push((Token::Str(text), start));
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            tokens.push((Token::Ident(chars[start..i].iter().collect()), start));
        } else {
            let rest: String = chars[i..chars.len().min(i + 3)].iter().collect();
            let punct = PUNCTS
                .iter()
                .find(|p| rest.starts_with(**p))
                .ok_or_else(|| syntax(source, &format!("unexpected `{c}`")))?;
            tokens.push((Token::Punct(punct), i));
            i += punct.chars().count();
        }
    }
    Ok(tokens)
}

#[derive(Debug)]
enum Expr {
    Literal(Value),
    /// With the char offset it is written at.
    Ident(String, usize),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Member(Box<Expr>, Box<Expr>, bool),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
}

struct Parser {
    tokens: Vec<Token>,
    offsets: Vec<usize>,
    position: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn eat(&mut self, punct: &str) -> bool {
        if matches!(self.peek(), Some(Token::Punct(p)) if *p == punct) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, punct: &str) -> Result<()> {
        if self.eat(punct) { Ok(()) } else { Err(Error::msg(format!("syntax error: expected `{punct}`"))) }
    }

    fn ternary(&mut self) -> Result<Expr> {
        let condition = self.binary(0)?;
        if !self.eat("?") {
            return Ok(condition);
        }
        let then = self.ternary()?;
        self.expect(":")?;
        let otherwise = self.ternary()?;
        Ok(Expr::Ternary(Box::new(condition), Box::new(then), Box::new(otherwise)))
    }

    /// Precedence climbing over the binary operators, loosest first.
    fn binary(&mut self, level: usize) -> Result<Expr> {
        const LEVELS: [&[&str]; 7] = [
            &["??"],
            &["||"],
            &["&&"],
            &["===", "!==", "==", "!="],
            &["<=", ">=", "<", ">"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        let Some(operators) = LEVELS.get(level) else { return self.unary() };
        let mut left = self.binary(level + 1)?;
        loop {
            let operator = match self.peek() {
                Some(Token::Punct(p)) if operators.contains(p) => *p,
                _ => return Ok(left),
            };
            self.position += 1;
            let right = self.binary(level + 1)?;
            left = Expr::Binary(operator, Box::new(left), Box::new(right));
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        for operator in ["!", "-", "+"] {
            if self.eat(operator) {
                return Ok(Expr::Unary(operator, Box::new(self.unary()?)));
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut expr = self.primary()?;
        loop {
            let optional = self.eat("?.");
            if optional || self.eat(".") {
                if optional && self.eat("[") {
                    let index = self.ternary()?;
                    self.expect("]")?;
                    expr = Expr::Member(Box::new(expr), Box::new(index), true);
                    continue;
                }
                let Some(Token::Ident(name)) = self.peek().cloned() else {
                    return Err(Error::msg("syntax error: expected a property name"));
                };
                self.position += 1;
                expr = Expr::Member(Box::new(expr), Box::new(Expr::Literal(Value::String(name))), optional);
            } else if self.eat("[") {
                let index = self.ternary()?;
                self.expect("]")?;
                expr = Expr::Member(Box::new(expr), Box::new(index), false);
            } else {
                return Ok(expr);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        let token = self.peek().cloned().ok_or_else(|| Error::msg("syntax error: unexpected end"))?;
        self.position += 1;
        Ok(match token {
            Token::Number(number) => Expr::Literal(number_value(number)),
            Token::Str(text) => Expr::Literal(Value::String(text)),
            Token::Ident(name) => match name.as_str() {
                "true" => Expr::Literal(Value::Bool(true)),
                "false" => Expr::Literal(Value::Bool(false)),
                "null" | "undefined" => Expr::Literal(Value::Null),
                _ => Expr::Ident(name, self.offsets[self.position - 1]),
            },
            Token::Punct("(") => {
                let inner = self.ternary()?;
                self.expect(")")?;
                inner
            }
            Token::Punct("[") => {
                let mut items = Vec::new();
                while !self.eat("]") {
                    items.push(self.ternary()?);
                    if !self.eat(",") {
                        self.expect("]")?;
                        break;
                    }
                }
                Expr::Array(items)
            }
            Token::Punct("{") => {
                let mut entries = Vec::new();
                while !self.eat("}") {
                    let key = match self.peek().cloned() {
                        Some(Token::Ident(key) | Token::Str(key)) => key,
                        _ => return Err(Error::msg("syntax error: expected a key")),
                    };
                    self.position += 1;
                    self.expect(":")?;
                    entries.push((key, self.ternary()?));
                    if !self.eat(",") {
                        self.expect("}")?;
                        break;
                    }
                }
                Expr::Object(entries)
            }
            Token::Punct(p) => return Err(Error::msg(format!("syntax error: unexpected `{p}`"))),
        })
    }
}

fn eval(ctx: &Context, expr: &Expr) -> Result<Value, Fault> {
    Ok(match expr {
        Expr::Literal(value) => value.clone(),
        Expr::Ident(name, offset) => {
            ctx.view(name.as_str()).map_err(|error| Fault { error: error.into(), offset: *offset })?
        }
        Expr::Array(items) => Value::Array(items.iter().map(|item| eval(ctx, item)).collect::<Result<_, Fault>>()?),
        Expr::Object(entries) => Value::Object(
            entries.iter().map(|(k, v)| Ok((k.clone(), eval(ctx, v)?))).collect::<Result<Map<_, _>, Fault>>()?,
        ),
        Expr::Member(object, key, optional) => {
            let object = eval(ctx, object)?;
            if object.is_null() {
                if *optional {
                    return Ok(Value::Null);
                }
                return Err(Error::msg(format!(
                    "cannot read properties of null (reading {})",
                    stringify(&eval(ctx, key)?)
                ))
                .into());
            }
            member(&object, &eval(ctx, key)?)
        }
        Expr::Unary(operator, operand) => {
            let value = eval(ctx, operand)?;
            match *operator {
                "!" => Value::Bool(!truthy(&value)),
                "-" => number_value(-to_number(&value)),
                _ => number_value(to_number(&value)),
            }
        }
        Expr::Binary(operator, left, right) => {
            let left = eval(ctx, left)?;
            match *operator {
                "&&" => return if truthy(&left) { eval(ctx, right) } else { Ok(left) },
                "||" => return if truthy(&left) { Ok(left) } else { eval(ctx, right) },
                "??" => return if left.is_null() { eval(ctx, right) } else { Ok(left) },
                _ => {}
            }
            binary(operator, &left, &eval(ctx, right)?)?
        }
        Expr::Ternary(condition, then, otherwise) => {
            if truthy(&eval(ctx, condition)?) {
                eval(ctx, then)?
            } else {
                eval(ctx, otherwise)?
            }
        }
    })
}

fn member(object: &Value, key: &Value) -> Value {
    match (object, key) {
        (Value::Object(map), key) => map.get(&stringify(key)).cloned().unwrap_or(Value::Null),
        (Value::Array(items), Value::String(name)) if name == "length" => Value::from(items.len()),
        (Value::Array(items), key) => index(key).and_then(|i| items.get(i)).cloned().unwrap_or(Value::Null),
        (Value::String(text), Value::String(name)) if name == "length" => Value::from(text.chars().count()),
        (Value::String(text), key) => {
            index(key).and_then(|i| text.chars().nth(i)).map(|c| Value::String(c.into())).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

fn index(key: &Value) -> Option<usize> {
    let number = to_number(key);
    (number >= 0.0 && number.fract() == 0.0).then_some(number as usize)
}

fn binary(operator: &str, left: &Value, right: &Value) -> Result<Value> {
    Ok(match operator {
        "+" if left.is_string() || right.is_string() => Value::String(stringify(left) + &stringify(right)),
        "+" => number_value(to_number(left) + to_number(right)),
        "-" => number_value(to_number(left) - to_number(right)),
        "*" => number_value(to_number(left) * to_number(right)),
        "/" => number_value(to_number(left) / to_number(right)),
        "%" => number_value(to_number(left) % to_number(right)),
        "===" => Value::Bool(strict_equal(left, right)),
        "!==" => Value::Bool(!strict_equal(left, right)),
        "==" => Value::Bool(loose_equal(left, right)),
        "!=" => Value::Bool(!loose_equal(left, right)),
        "<" | ">" | "<=" | ">=" => {
            let ordering = match (left, right) {
                (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
                _ => to_number(left).partial_cmp(&to_number(right)),
            };
            Value::Bool(ordering.is_some_and(|ordering| match operator {
                "<" => ordering.is_lt(),
                ">" => ordering.is_gt(),
                "<=" => ordering.is_le(),
                _ => ordering.is_ge(),
            }))
        }
        other => return Err(Error::msg(format!("unsupported operator {other}"))),
    })
}

fn strict_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(_), Value::Number(_)) => to_number(left) == to_number(right),
        (Value::Array(_) | Value::Object(_), _) => false,
        _ => left == right,
    }
}

fn loose_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (
            Value::Number(_) | Value::String(_) | Value::Bool(_),
            Value::Number(_) | Value::String(_) | Value::Bool(_),
        ) if std::mem::discriminant(left) != std::mem::discriminant(right) => to_number(left) == to_number(right),
        _ => strict_equal(left, right),
    }
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

fn to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(value) => f64::from(u8::from(*value)),
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) if text.trim().is_empty() => 0.0,
        Value::String(text) => text.trim().parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

fn stringify(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(text) => text.clone(),
        Value::Number(number) => match number.as_f64() {
            Some(n) if n.fract() == 0.0 && n.abs() < 1e21 => format!("{}", n as i64),
            Some(n) => n.to_string(),
            None => number.to_string(),
        },
        Value::Array(items) => items.iter().map(stringify).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Bool(value) => value.to_string(),
    }
}

/// Integral numbers stay integers in JSON.
fn number_value(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() < 9.0e15 {
        Value::from(number as i64)
    } else {
        Number::from_f64(number).map_or(Value::Null, Value::Number)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    async fn root() -> Context {
        let root = Context::new();
        root.provide_as("settings", json!({ "enabled": true, "port": 8080, "tags": ["a", "b"], "name": "app" }))
            .unwrap();
        root
    }

    #[tokio::test]
    async fn evaluates_javascript_like_expressions() {
        let root = root().await;
        let cases = [
            ("1 + 2 * 3", json!(7)),
            ("'#' + 1", json!("#1")),
            ("!settings.enabled", json!(false)),
            ("settings.port + 1", json!(8081)),
            ("settings.tags[1] + settings.tags.length", json!("b2")),
            ("settings.missing ?? 'fallback'", json!("fallback")),
            ("settings.missing?.deep", json!(null)),
            ("settings.port > 1000 && settings.name === 'app' ? 'big' : 'small'", json!("big")),
            ("[1, 'x', { a: settings.port }]", json!([1, "x", { "a": 8080 }])),
            ("1 == '1' && 1 !== '1' && null == undefined", json!(true)),
            ("7 % 4 - -1 / 2", json!(3.5)),
        ];
        for (source, expected) in cases {
            assert_eq!(evaluate(&root, source).unwrap(), expected, "{source}");
        }
    }

    #[tokio::test]
    async fn reports_syntax_errors_and_access_errors() {
        let root = root().await;
        assert!(evaluate(&root, "1 +").is_err());
        assert!(evaluate(&root, "'open").is_err());
        assert!(evaluate(&root, "unknown.value").is_err());
        let interpolated = interpolate(&root, &json!({ "a": [{ "__jsExpr": "settings.port" }], "b": 1 })).unwrap();
        assert_eq!(interpolated, json!({ "a": [8080], "b": 1 }));
    }
}
