use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Compound(BTreeMap<String, Value>),
    List(Vec<Value>),
    Scalar(String),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_compound()?.get(key)
    }

    pub fn as_compound(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Compound(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_compound_mut(&mut self) -> Option<&mut BTreeMap<String, Value>> {
        match self {
            Value::Compound(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Scalar(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    message: String,
    line: usize,
    column: usize,
}

impl ParseError {
    fn new(message: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            message: message.into(),
            line,
            column,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} (строка {}, столбец {})",
            self.message, self.line, self.column
        )
    }
}

impl Error for ParseError {}

pub fn parse(source: &str) -> Result<Value, ParseError> {
    Parser::new(source).parse()
}

pub fn stringify(value: &Value) -> String {
    match value {
        Value::Compound(values) => {
            let contents = values
                .iter()
                .map(|(key, value)| format!("{}: {}", stringify_key(key), stringify(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {contents} }}")
        }
        Value::List(values) => {
            let contents = values.iter().map(stringify).collect::<Vec<_>>().join(", ");
            format!("[{contents}]")
        }
        Value::Scalar(value) if is_bare_literal(value) => value.clone(),
        Value::Scalar(value) => quote(value),
    }
}

fn stringify_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-.+".contains(character))
    {
        key.to_owned()
    } else {
        quote(key)
    }
}

fn is_bare_literal(value: &str) -> bool {
    matches!(value, "true" | "false")
        || value
            .trim_end_matches(|character: char| {
                matches!(
                    character,
                    'b' | 'B' | 's' | 'S' | 'l' | 'L' | 'f' | 'F' | 'd' | 'D'
                )
            })
            .parse::<f64>()
            .is_ok()
}

fn quote(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000C}' => escaped.push_str("\\f"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04X}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

struct Parser<'a> {
    source: &'a str,
    offset: usize,
    line: usize,
    column: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            line: 1,
            column: 1,
        }
    }

    fn parse(mut self) -> Result<Value, ParseError> {
        self.skip_trivia()?;
        let value = self.parse_value()?;
        self.skip_trivia()?;
        if self.peek().is_some() {
            return Err(self.error("лишние данные после корневого значения"));
        }
        Ok(value)
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        self.skip_trivia()?;
        match self.peek() {
            Some('{') => self.parse_compound(),
            Some('[') => self.parse_list(),
            Some('"') | Some('\'') => self.parse_quoted().map(Value::Scalar),
            Some(_) => self.parse_bare_value().map(Value::Scalar),
            None => Err(self.error("ожидалось значение")),
        }
    }

    fn parse_compound(&mut self) -> Result<Value, ParseError> {
        self.expect('{')?;
        let mut values = BTreeMap::new();

        loop {
            self.skip_trivia()?;
            if self.consume_if('}') {
                return Ok(Value::Compound(values));
            }

            let key = self.parse_key()?;
            self.skip_whitespace_and_comments()?;
            self.expect(':')?;
            let value = self.parse_value()?;
            values.insert(key, value);

            self.skip_trivia()?;
            self.consume_if(',');
        }
    }

    fn parse_list(&mut self) -> Result<Value, ParseError> {
        self.expect('[')?;
        self.skip_whitespace_and_comments()?;

        // Standard SNBT also has typed arrays such as [I; 1, 2, 3].
        let saved = (self.offset, self.line, self.column);
        if matches!(self.peek(), Some('B' | 'I' | 'L' | 'b' | 'i' | 'l')) {
            self.bump();
            self.skip_whitespace_and_comments()?;
            if !self.consume_if(';') {
                (self.offset, self.line, self.column) = saved;
            }
        }

        let mut values = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.consume_if(']') {
                return Ok(Value::List(values));
            }

            values.push(self.parse_value()?);
            self.skip_trivia()?;
            self.consume_if(',');
        }
    }

    fn parse_key(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Some('"') | Some('\'') => self.parse_quoted(),
            Some(_) => {
                let start = self.offset;
                while let Some(character) = self.peek() {
                    if character.is_whitespace()
                        || matches!(character, ':' | ',' | '{' | '}' | '[' | ']')
                    {
                        break;
                    }
                    self.bump();
                }
                if self.offset == start {
                    Err(self.error("ожидалось имя поля"))
                } else {
                    Ok(self.source[start..self.offset].to_owned())
                }
            }
            None => Err(self.error("неожиданный конец объекта")),
        }
    }

    fn parse_bare_value(&mut self) -> Result<String, ParseError> {
        let start = self.offset;
        while let Some(character) = self.peek() {
            if character.is_whitespace() || matches!(character, ',' | '}' | ']') {
                break;
            }
            self.bump();
        }
        if self.offset == start {
            Err(self.error("ожидалось значение"))
        } else {
            Ok(self.source[start..self.offset].to_owned())
        }
    }

    fn parse_quoted(&mut self) -> Result<String, ParseError> {
        let quote = self.bump().ok_or_else(|| self.error("ожидалась строка"))?;
        let mut result = String::new();

        loop {
            match self.bump() {
                Some(character) if character == quote => return Ok(result),
                Some('\\') => result.push(self.parse_escape()?),
                Some(character) => result.push(character),
                None => return Err(self.error("незакрытая строка")),
            }
        }
    }

    fn parse_escape(&mut self) -> Result<char, ParseError> {
        match self.bump() {
            Some('"') => Ok('"'),
            Some('\'') => Ok('\''),
            Some('\\') => Ok('\\'),
            Some('/') => Ok('/'),
            Some('b') => Ok('\u{0008}'),
            Some('f') => Ok('\u{000C}'),
            Some('n') => Ok('\n'),
            Some('r') => Ok('\r'),
            Some('t') => Ok('\t'),
            Some('u') => {
                let mut value = 0_u32;
                for _ in 0..4 {
                    let digit = self
                        .bump()
                        .and_then(|character| character.to_digit(16))
                        .ok_or_else(|| self.error("некорректная Unicode-последовательность"))?;
                    value = value * 16 + digit;
                }
                char::from_u32(value).ok_or_else(|| self.error("некорректный Unicode-символ"))
            }
            Some(character) => Ok(character),
            None => Err(self.error("незавершённая escape-последовательность")),
        }
    }

    fn skip_trivia(&mut self) -> Result<(), ParseError> {
        loop {
            self.skip_whitespace_and_comments()?;
            if !self.consume_if(',') {
                return Ok(());
            }
        }
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<(), ParseError> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }

            if self.remaining().starts_with("//") {
                while !matches!(self.bump(), Some('\n') | None) {}
            } else if self.remaining().starts_with("/*") {
                self.bump();
                self.bump();
                while !self.remaining().starts_with("*/") {
                    if self.bump().is_none() {
                        return Err(self.error("незакрытый блочный комментарий"));
                    }
                }
                self.bump();
                self.bump();
            } else if self.peek() == Some('#') {
                while !matches!(self.bump(), Some('\n') | None) {}
            } else {
                return Ok(());
            }
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), ParseError> {
        match self.bump() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => {
                Err(self.error(format!("ожидался символ '{expected}', найден '{actual}'")))
            }
            None => Err(self.error(format!("ожидался символ '{expected}', найден конец файла"))),
        }
    }

    fn consume_if(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn remaining(&self) -> &str {
        &self.source[self.offset..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        if character == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(character)
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError::new(message, self.line, self.column)
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, parse, stringify};

    #[test]
    fn parses_ftb_style_without_commas() {
        let value = parse(
            r#"{
                id: "ABC"
                enabled: true
                dependencies: ["ONE" "TWO"]
                task: { type: "item", count: 2 }
            }"#,
        )
        .unwrap();

        assert_eq!(value.get("id").and_then(Value::as_str), Some("ABC"));
        assert_eq!(
            value
                .get("dependencies")
                .and_then(Value::as_list)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn parses_namespaced_values_typed_arrays_and_comments() {
        let value = parse(
            r#"{
                item: minecraft:stone
                bytes: [B; 1b, 2b]
                // FTB files may contain comments.
                text: "line\n\u0041"
            }"#,
        )
        .unwrap();

        assert_eq!(
            value.get("item").and_then(Value::as_str),
            Some("minecraft:stone")
        );
        assert_eq!(
            value.get("bytes").and_then(Value::as_list).unwrap().len(),
            2
        );
        assert_eq!(value.get("text").and_then(Value::as_str), Some("line\nA"));
    }

    #[test]
    fn stringifies_values_without_changing_scalar_types() {
        let value = parse(
            r#"{ id: "minecraft:stone", title: "Алмаз", count: 2L, enabled: true, tags: ["a" "b"] }"#,
        )
        .unwrap();
        let output = stringify(&value);
        let reparsed = parse(&output).unwrap();
        assert_eq!(reparsed, value);
    }
}
