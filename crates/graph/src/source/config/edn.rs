use super::name_format::hex_value;

/// One top-level configuration value, reduced to the shapes this module interprets.
pub(super) enum Value {
    /// `:keyword`
    Keyword(String),
    /// `"string"`, with escapes resolved.
    Text(String),
    /// A map, vector, list, or set, with its raw source text, whether it holds any element, and —
    /// for an ordered collection whose every element is a string — those elements.
    Collection {
        source: String,
        empty: bool,
        strings: Option<Vec<String>>,
    },
    /// A number, symbol, or boolean, which is skipped unread.
    Other,
}

/// Cursor over the configuration text.
///
/// It is a scanner, not a general EDN reader: it recognizes strings, keywords, and balanced
/// collections well enough to find the top-level keys and to skip nested values without confusing
/// a nested key for a top-level one.
pub(super) struct Reader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    pub(super) fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub(super) fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    pub(super) fn bump(&mut self) {
        self.pos += 1;
    }

    /// Skip whitespace, commas, and `;` comments.
    pub(super) fn skip_trivia(&mut self) {
        while let Some(byte) = self.peek() {
            match byte {
                b' ' | b'\t' | b'\r' | b'\n' | b',' => self.bump(),
                b';' => {
                    while let Some(byte) = self.peek() {
                        self.bump();
                        if byte == b'\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    /// Read a keyword without its leading colon. The caller has already checked for `:`.
    pub(super) fn read_keyword(&mut self) -> String {
        self.bump();
        let start = self.pos;
        while self.peek().is_some_and(|byte| !is_delimiter(byte)) {
            self.bump();
        }
        self.text[start..self.pos].to_owned()
    }

    pub(super) fn read_value(&mut self) -> Result<Value, String> {
        match self.peek() {
            None => Err("expected a value, found the end of the file".to_owned()),
            Some(b'"') => self.read_string().map(Value::Text),
            Some(b':') => Ok(Value::Keyword(self.read_keyword())),
            Some(b'{') => self.read_collection(b'}'),
            Some(b'[') => self.read_collection(b']'),
            Some(b'(') => self.read_collection(b')'),
            Some(b'#') => self.read_set(),
            Some(_) => {
                let start = self.pos;
                self.read_atom();
                // `}`/`]`/`)` and commas are delimiters, so a key without a value would otherwise
                // consume nothing and be accepted as an empty value.
                if self.pos == start {
                    return Err("expected a value".to_owned());
                }
                Ok(Value::Other)
            }
        }
    }

    /// Read a balanced collection and keep its raw source text.
    fn read_collection(&mut self, closer: u8) -> Result<Value, String> {
        let start = self.pos;
        let (empty, strings) = self.skip_collection(closer)?;
        Ok(Value::Collection {
            source: self.text[start..self.pos].to_owned(),
            empty,
            strings,
        })
    }

    /// Read a set literal `#{...}` and keep its raw source text.
    ///
    /// `#` introduces a set and nothing else here. A dispatch tag such as `#inst` or a pattern such
    /// as `#"regex"` is refused instead of being skipped as if it were an atom, because a value
    /// this module does not understand must not pass as a value it does.
    fn read_set(&mut self) -> Result<Value, String> {
        let start = self.pos;
        self.bump();
        if self.peek() != Some(b'{') {
            return Err("expected `{` after `#`, which only introduces a set".to_owned());
        }
        let (empty, _) = self.skip_collection(b'}')?;
        // A set has no order to report, so its elements are not offered as a string sequence even
        // when every one of them is a string.
        Ok(Value::Collection {
            source: self.text[start..self.pos].to_owned(),
            empty,
            strings: None,
        })
    }

    /// Read a quoted string and resolve its escapes.
    fn read_string(&mut self) -> Result<String, String> {
        self.bump();
        let mut out: Vec<u8> = Vec::new();

        loop {
            let Some(byte) = self.peek() else {
                return Err("string is not closed".to_owned());
            };
            self.bump();
            match byte {
                b'"' => break,
                b'\\' => self.read_escape(&mut out)?,
                _ => out.push(byte),
            }
        }

        String::from_utf8(out).map_err(|_| "string is not valid UTF-8".to_owned())
    }

    fn read_escape(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        let Some(byte) = self.peek() else {
            return Err("string ends inside an escape".to_owned());
        };
        self.bump();
        let decoded = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'u' => return self.read_unicode_escape(out),
            other => {
                return Err(format!(
                    "unsupported escape `\\{}` in a string",
                    other as char
                ));
            }
        };
        out.push(decoded as u8);
        Ok(())
    }

    /// Read the four hexadecimal digits of `\uXXXX`.
    fn read_unicode_escape(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        let mut code = 0_u32;
        for _ in 0..4 {
            let digit = self
                .peek()
                .and_then(hex_value)
                .ok_or_else(|| "`\\u` must be followed by four hexadecimal digits".to_owned())?;
            self.bump();
            code = code * 16 + u32::from(digit);
        }
        let character = char::from_u32(code)
            .ok_or_else(|| format!("`\\u{code:04X}` is not a Unicode scalar value"))?;
        let mut buffer = [0_u8; 4];
        out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        Ok(())
    }

    /// Skip one balanced collection, starting at its opening delimiter.
    ///
    /// Returns whether the collection held at least one element, and — for the two ordered
    /// collection shapes, a vector and a list — the elements when every one of them is a quoted
    /// string. A map or a set answers `None` there: a map's elements are pairs, and a set has no
    /// order to report.
    fn skip_collection(&mut self, closer: u8) -> Result<(bool, Option<Vec<String>>), String> {
        self.bump();
        let mut expected = vec![closer];
        let mut empty = true;
        let mut strings = if matches!(closer, b']' | b')') {
            Some(Vec::new())
        } else {
            None
        };
        while let Some(closer) = expected.last().copied() {
            self.skip_trivia();
            match self.peek() {
                None => {
                    return Err(format!(
                        "collection is not closed, expected `{}`",
                        closer as char
                    ));
                }
                Some(b'"') => {
                    let element = self.read_string()?;
                    if let Some(elements) = strings.as_mut() {
                        elements.push(element);
                    }
                    empty = false;
                }
                Some(b'{') => {
                    expected.push(b'}');
                    self.bump();
                    empty = false;
                    strings = None;
                }
                Some(b'[') => {
                    expected.push(b']');
                    self.bump();
                    empty = false;
                    strings = None;
                }
                Some(b'(') => {
                    expected.push(b')');
                    self.bump();
                    empty = false;
                    strings = None;
                }
                Some(byte) if byte == closer => {
                    expected.pop();
                    self.bump();
                }
                Some(b'}') | Some(b']') | Some(b')') => {
                    return Err("mismatched closing delimiter".to_owned());
                }
                Some(_) => {
                    let start = self.pos;
                    self.read_atom();
                    if self.pos == start {
                        return Err("expected a value".to_owned());
                    }
                    empty = false;
                    strings = None;
                }
            }
        }
        Ok((empty, strings))
    }

    /// Consume an unquoted atom such as a number, symbol, or boolean.
    fn read_atom(&mut self) {
        while self.peek().is_some_and(|byte| !is_delimiter(byte)) {
            self.bump();
        }
    }
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\r' | b'\n' | b',' | b';' | b'"' | b'{' | b'}' | b'[' | b']' | b'(' | b')'
    )
}

#[cfg(test)]
mod tests {
    use super::super::{GraphConfig, SourceError};
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("graph/logseq/config.edn")
    }

    fn parse(text: &str) -> Result<GraphConfig, SourceError> {
        GraphConfig::parse(text, &path())
    }

    #[test]
    fn set_value_under_an_unknown_key_is_skipped() {
        let config = parse("{:a #{:x}}").expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn logseq_default_sets_are_skipped() {
        // Both keys are in Logseq's own `config.edn` template, so a default graph writes them.
        for text in [
            "{:block-hidden-properties #{:lt-id}}",
            "{:property-pages/excludelist #{:type :tags :last}}",
            "{:property/separated-by-commas #{:authors :type}}",
        ] {
            let config = parse(text).unwrap_or_else(|error| panic!("{text}: {error}"));
            assert_eq!(config, GraphConfig::default(), "{text}");
        }
    }

    #[test]
    fn nested_sets_are_skipped_balanced() {
        let config = parse("{:a #{#{:x} {:k #{:y}}}}").expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn braces_inside_strings_do_not_close_a_set() {
        // A scanner that counted braces without honouring strings would stop at the first `}`.
        let config = parse(r#"{:a #{"}" "{" }}"#).expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn empty_set_is_skipped() {
        let config = parse("{:a #{}}").expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn unclosed_set_is_rejected() {
        let error = parse("{:a #{:x}").expect_err("rejected");
        match error {
            SourceError::InvalidConfig { detail, .. } => {
                assert!(detail.contains("not closed"), "unexpected detail: {detail}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn hash_that_does_not_introduce_a_set_is_rejected() {
        // The scanner has no tagged-literal or regex reader, so `#inst`, `#"re"`, and a bare `#`
        // are refused rather than silently dropped after the `#`.
        for text in [
            "{:a #}",
            "{:a # }",
            "{:a #inst \"2020-01-01\"}",
            "{:a #\"re\"}",
        ] {
            let error = parse(text).expect_err("not skipped");
            match error {
                SourceError::InvalidConfig { detail, .. } => assert!(
                    detail.contains("after `#`"),
                    "unexpected detail for {text}: {detail}"
                ),
                other => panic!("unexpected error for {text}: {other}"),
            }
        }
    }
}
