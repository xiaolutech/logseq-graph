//! The parser for the simple-query grammar.
//!
//! It is a hand-written recursive descent over the query text, and it is the only code that reads a
//! query as text. The two steps are deliberately separate: the first turns the text into a bare
//! tree of forms and atoms with no meaning attached, the second lowers that tree into
//! [`QueryExpr`] by looking at each form's name. Splitting them keeps the syntax rules — what a form
//! is, where a string ends — apart from the vocabulary — which forms exist, and what their arguments
//! may be.
//!
//! Nothing here knows about pages, blocks, or SQL. It knows that `#x` and `[[x]]` are two spellings
//! of one reference, that `+2w` is fourteen days, and that `(sort-by …)` is a form this build
//! refuses. Everything else is somebody else's question.

use super::{DateOperand, JournalDate, Priority, QueryError, QueryExpr, TaskMarker};

/// The query text the rest of this module reads, and how far it has been read.
struct Parser<'a> {
    input: &'a str,
    position: usize,
}

/// A parsed but not yet interpreted piece of the query text.
///
/// The distinction between [`Str`](Self::Str) and [`Word`](Self::Word) is kept even though both
/// lower to the same text, because it is the only place the grammar's answer differs from the text:
/// a quoted string is one argument whatever it contains, so `(property k "a b")` has two arguments
/// and `(property k a b)` has three. Lowering may then treat both as text.
#[derive(Debug)]
enum Sexp {
    Form {
        /// Byte offset of the opening parenthesis, for error positions.
        position: usize,
        /// Form name as written; callers compare it case-insensitively.
        name: String,
        args: Vec<Sexp>,
    },
    /// `[[name]]`, already trimmed and known non-empty.
    PageRef(String),
    /// `#tag`, with the `#` removed.
    Tag(String),
    /// `"text"`, escapes decoded.
    Str(String),
    /// A bare word: everything up to whitespace or a delimiter.
    Word(String),
}

/// Parse one query.
pub(super) fn parse(input: &str) -> Result<QueryExpr, QueryError> {
    let mut parser = Parser { input, position: 0 };
    let mut forms = Vec::new();
    loop {
        parser.skip_whitespace();
        if parser.at_end() {
            break;
        }
        forms.push(parser.parse_sexp(1)?);
    }
    match forms.len() {
        // An empty query asks nothing, and "nothing" is not an answer: a caller that meant it can
        // say what it meant. Answering it as an empty tree would make a missing query string
        // indistinguishable from a query that matched no block.
        0 => Err(QueryError::Syntax {
            position: 0,
            detail:
                "this query holds no expression, so there is nothing to answer; write at least \
                     one form, a `[[page]]` reference, a `#tag`, or a word"
                    .to_owned(),
        }),
        1 => lower(&forms[0]),
        // Adjacent top-level expressions are an implicit `and`: `[[a]] [[b]]` means both.
        _ => Ok(QueryExpr::And(
            forms.iter().map(lower).collect::<Result<Vec<_>, _>>()?,
        )),
    }
}

impl<'a> Parser<'a> {
    fn at_end(&self) -> bool {
        self.position >= self.input.len()
    }

    fn rest(&self) -> &'a str {
        &self.input[self.position..]
    }

    fn skip_whitespace(&mut self) {
        let input = self.input;
        while let Some(character) = input[self.position..].chars().next() {
            if !character.is_whitespace() {
                break;
            }
            self.position += character.len_utf8();
        }
    }

    /// Read one expression: a form, a reference, a string, or a word.
    fn parse_sexp(&mut self, depth: usize) -> Result<Sexp, QueryError> {
        self.skip_whitespace();
        let position = self.position;
        let rest = self.rest();
        // `[[` is checked before the `[` that opens a Datalog vector, and it is the only thing that
        // distinguishes the two: one extra bracket is the difference between a page reference and a
        // form this build refuses.
        if rest.starts_with("[[") {
            return self.parse_page_ref();
        }
        let Some(character) = rest.chars().next() else {
            return Err(QueryError::Syntax {
                position,
                detail: "the query ends where an expression was expected".to_owned(),
            });
        };
        match character {
            '(' => self.parse_form(depth),
            '[' => Err(QueryError::Unsupported {
                form: self.delimited_group('[', ']')?,
            }),
            '{' => Err(QueryError::Unsupported {
                form: self.delimited_group('{', '}')?,
            }),
            '"' => Ok(Sexp::Str(self.parse_string()?)),
            '#' => self.parse_tag(),
            ')' | ']' | '}' => Err(QueryError::Syntax {
                position,
                detail: format!(
                    "`{character}` closes a form that was not opened; check the parentheses of this \
                     query"
                ),
            }),
            _ => Ok(Sexp::Word(self.take_word().to_owned())),
        }
    }

    /// Read one `(name arg…)` form.
    fn parse_form(&mut self, depth: usize) -> Result<Sexp, QueryError> {
        let position = self.position;
        if depth > QueryExpr::MAX_DEPTH {
            return Err(QueryError::Syntax {
                position,
                detail: format!(
                    "this form nests deeper than the {} levels a query may nest",
                    QueryExpr::MAX_DEPTH
                ),
            });
        }
        // The opening parenthesis.
        self.position += 1;
        self.skip_whitespace();
        let open_at = self.position;
        match self.rest().chars().next() {
            None => {
                return Err(QueryError::Syntax {
                    position,
                    detail: "this form is never closed".to_owned(),
                })
            }
            Some(')') => {
                return Err(QueryError::Syntax {
                    position,
                    detail:
                        "`()` is a form with no name; every form starts with its name, such as \
                             `(and …)` or `(page …)`"
                            .to_owned(),
                })
            }
            Some(character) if is_delimiter(character) => {
                return Err(QueryError::Syntax {
                    position: open_at,
                    detail: "a form starts with its name, written as a bare word".to_owned(),
                })
            }
            Some(_) => {}
        }
        let name = self.take_word().to_owned();
        let mut args = Vec::new();
        loop {
            self.skip_whitespace();
            match self.rest().chars().next() {
                None => {
                    return Err(QueryError::Syntax {
                        position,
                        detail: format!("the form `({name} …)` is never closed"),
                    })
                }
                Some(')') => {
                    self.position += 1;
                    return Ok(Sexp::Form {
                        position,
                        name,
                        args,
                    });
                }
                Some(_) => args.push(self.parse_sexp(depth + 1)?),
            }
        }
    }

    /// Read `[[name]]`.
    fn parse_page_ref(&mut self) -> Result<Sexp, QueryError> {
        let start = self.position;
        let inner = start + "[[".len();
        let Some(length) = self.input[inner..].find("]]") else {
            return Err(QueryError::Syntax {
                position: start,
                detail: "this page reference is never closed by `]]`".to_owned(),
            });
        };
        let name = self.input[inner..inner + length].trim();
        self.position = inner + length + "]]".len();
        if name.is_empty() {
            return Err(QueryError::Syntax {
                position: start,
                detail: "`[[]]` names no page".to_owned(),
            });
        }
        Ok(Sexp::PageRef(name.to_owned()))
    }

    /// Read `#tag`.
    fn parse_tag(&mut self) -> Result<Sexp, QueryError> {
        let start = self.position;
        // The `#`.
        self.position += 1;
        let tag = self.take_word().to_owned();
        if tag.is_empty() {
            return Err(QueryError::Syntax {
                position: start,
                detail: "`#` names no tag; write the tag's name directly after it, such as `#book`"
                    .to_owned(),
            });
        }
        Ok(Sexp::Tag(tag))
    }

    /// Read `"text"`, decoding escapes.
    fn parse_string(&mut self) -> Result<String, QueryError> {
        let start = self.position;
        // The opening quote.
        self.position += 1;
        let mut text = String::new();
        loop {
            let Some(character) = self.rest().chars().next() else {
                return Err(QueryError::Syntax {
                    position: start,
                    detail: "this string is never closed by a quote".to_owned(),
                });
            };
            self.position += character.len_utf8();
            match character {
                '"' => return Ok(text),
                '\\' => {
                    let escape = self.position - 1;
                    let Some(escaped) = self.rest().chars().next() else {
                        return Err(QueryError::Syntax {
                            position: escape,
                            detail: "this string ends with a backslash".to_owned(),
                        });
                    };
                    self.position += escaped.len_utf8();
                    text.push(match escaped {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        // Mapping an unknown escape onto its own character would silently change
                        // what the caller searched for, so it is refused by name instead.
                        other => {
                            return Err(QueryError::Syntax {
                                position: escape,
                                detail: format!(
                                    "`\\{other}` is not an escape this parser knows; write `\\\\` \
                                     for a backslash, `\\\"` for a quote, or the character itself"
                                ),
                            })
                        }
                    });
                }
                other => text.push(other),
            }
        }
    }

    /// Read a bare word: everything up to whitespace or a delimiter.
    fn take_word(&mut self) -> &'a str {
        let input = self.input;
        let start = self.position;
        let mut end = start;
        for (offset, character) in input[start..].char_indices() {
            if is_delimiter(character) {
                break;
            }
            end = start + offset + character.len_utf8();
        }
        self.position = end;
        &input[start..end]
    }

    /// Take the whole `[ … ]` or `{ … }` group as the text of an unsupported form.
    ///
    /// The group is returned verbatim so the caller can see what was refused: `[:find ?b :where …]`
    /// and `{:title "x" :query […]}` are both queries of a kind this build does not run, and echoing
    /// the text is more use than naming a shape. Nesting is counted so the group ends where it
    /// really ends; it is not parsed, because there is nothing here that could parse it.
    fn delimited_group(&mut self, open: char, close: char) -> Result<String, QueryError> {
        let start = self.position;
        let mut depth = 0usize;
        for (offset, character) in self.input[start..].char_indices() {
            if character == open {
                depth += 1;
            } else if character == close {
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + character.len_utf8();
                    self.position = end;
                    return Ok(self.input[start..end].to_owned());
                }
            }
        }
        Err(QueryError::Syntax {
            position: start,
            detail: format!("this group is never closed by a `{close}`"),
        })
    }
}

/// Whether a character ends a bare word.
///
/// A word ends where the next expression could begin, so every delimiter is a character that means
/// something to the grammar. `#` is not one: it only introduces a tag at the start of a token, and a
/// `#` inside a word is part of the word.
fn is_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, '(' | ')' | '[' | ']' | '{' | '}' | '"')
}

/// Lower one parsed piece into the tree it denotes.
fn lower(sexp: &Sexp) -> Result<QueryExpr, QueryError> {
    match sexp {
        Sexp::Form {
            position,
            name,
            args,
        } => lower_form(*position, name, args),
        Sexp::PageRef(name) | Sexp::Tag(name) => Ok(QueryExpr::PageRef(name.clone())),
        Sexp::Str(text) | Sexp::Word(text) => Ok(QueryExpr::Content(text.clone())),
    }
}

/// Lower one form by its name.
fn lower_form(position: usize, name: &str, args: &[Sexp]) -> Result<QueryExpr, QueryError> {
    let form = name.to_ascii_lowercase();
    match form.as_str() {
        "and" | "or" => {
            let children = lower_all(position, name, args)?;
            Ok(if form == "and" {
                QueryExpr::And(children)
            } else {
                QueryExpr::Or(children)
            })
        }
        "not" => {
            // `(not a b)` is one predicate — not both `a` and `b` match — which is the negation of
            // an `and`, never of an `or`. Normalizing here means the executor never has to ask what
            // a multi-child `not` meant.
            let inner = match args {
                [] => return Err(arity(position, name, "at least one expression", 0)),
                [single] => lower(single)?,
                many => QueryExpr::And(many.iter().map(lower).collect::<Result<Vec<_>, _>>()?),
            };
            Ok(QueryExpr::Not(Box::new(inner)))
        }
        "task" => {
            let valid = listed_or(
                &TaskMarker::ALL
                    .iter()
                    .map(|marker| marker.as_str())
                    .collect::<Vec<_>>(),
            );
            let markers = arguments(position, name, args, 1, None)?
                .into_iter()
                .map(|argument| {
                    TaskMarker::parse_dsl(&argument).ok_or_else(|| QueryError::InvalidArgument {
                        form: form.clone(),
                        argument,
                        detail: format!("expected a task marker: {valid}, in any casing"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(QueryExpr::Task(markers))
        }
        "priority" => {
            let valid = listed_or(
                &Priority::ALL
                    .iter()
                    .map(|priority| priority.as_str())
                    .collect::<Vec<_>>(),
            );
            let priorities = arguments(position, name, args, 1, None)?
                .into_iter()
                .map(|argument| {
                    Priority::parse_dsl(&argument).ok_or_else(|| QueryError::InvalidArgument {
                        form: form.clone(),
                        argument,
                        detail: format!("expected a priority letter: {valid}, in any casing"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(QueryExpr::Priority(priorities))
        }
        "property" | "page-property" => {
            let texts = arguments(position, name, args, 1, Some(2))?;
            let key = property_key(&form, &texts[0])?;
            let value = texts.get(1).cloned();
            Ok(if form == "property" {
                QueryExpr::Property { key, value }
            } else {
                QueryExpr::PageProperty { key, value }
            })
        }
        "page-tags" => {
            let tags = arguments(position, name, args, 1, None)?
                .into_iter()
                .map(|argument| {
                    let tag = argument.strip_prefix('#').unwrap_or(&argument);
                    if tag.is_empty() {
                        return Err(QueryError::InvalidArgument {
                            form: form.clone(),
                            argument,
                            detail: "expected a tag name".to_owned(),
                        });
                    }
                    Ok(tag.to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(QueryExpr::PageTags(tags))
        }
        "namespace" => Ok(QueryExpr::Namespace(only_argument(position, name, args)?)),
        "page" => Ok(QueryExpr::Page(only_argument(position, name, args)?)),
        "journal" => {
            if !args.is_empty() {
                return Err(arity(position, name, "no argument", args.len()));
            }
            Ok(QueryExpr::Journal)
        }
        "between" => {
            let operands = arguments(position, name, args, 2, Some(2))?;
            Ok(QueryExpr::Between(
                date_operand(&form, &args[0], &operands[0])?,
                date_operand(&form, &args[1], &operands[1])?,
            ))
        }
        "page-ref" => Ok(QueryExpr::PageRef(only_argument(position, name, args)?)),
        // Everything else is a form this build does not run, and the name is echoed as written so a
        // caller can see what was refused. Every form of Logseq's simple query that this build
        // deliberately leaves out is here: `sort-by` (no creation or update time exists in the
        // index), `sample` (a random order cannot be pinned by a test), `all-page-tags` (the
        // semantics of what it returns are undecided), and `scheduled` / `deadline` (no fact layer
        // holds a `<…>` date). Naming them is better than calling them unknown, because a caller
        // that wrote `(sort-by created-at)` has a different problem from one that wrote
        // `(sort-byyy)`.
        _ => Err(QueryError::Unsupported {
            form: name.to_owned(),
        }),
    }
}

/// Lower every argument as its own expression.
fn lower_all(position: usize, name: &str, args: &[Sexp]) -> Result<Vec<QueryExpr>, QueryError> {
    if args.is_empty() {
        return Err(arity(position, name, "at least one expression", 0));
    }
    args.iter().map(lower).collect()
}

/// The text of every argument, after checking how many there are.
fn arguments(
    position: usize,
    name: &str,
    args: &[Sexp],
    least: usize,
    most: Option<usize>,
) -> Result<Vec<String>, QueryError> {
    let count = args.len();
    if count < least || most.is_some_and(|most| count > most) {
        let expected = match most {
            Some(most) if most == least => format!("exactly {least}"),
            Some(most) => format!("{least} to {most}"),
            None => format!("at least {least}"),
        };
        return Err(arity(position, name, &expected, count));
    }
    args.iter()
        .map(|argument| {
            text_of(argument)
                .map(str::to_owned)
                .ok_or_else(|| QueryError::Syntax {
                    position: sexp_position(argument),
                    detail: format!(
                        "`({name} …)` takes words, quoted strings, and references here"
                    ),
                })
        })
        .collect()
}

/// The text of the one argument a single-argument form takes.
fn only_argument(position: usize, name: &str, args: &[Sexp]) -> Result<String, QueryError> {
    arguments(position, name, args, 1, Some(1))?
        .into_iter()
        .next()
        .ok_or_else(|| arity(position, name, "exactly one", 0))
}

/// The text a piece contributes where a word is expected.
///
/// A reference contributes its name without the brackets or the `#`: `(namespace [[a/b]])` and
/// `(namespace a/b)` say the same thing, because both name the page.
fn text_of(sexp: &Sexp) -> Option<&str> {
    match sexp {
        Sexp::PageRef(name) | Sexp::Tag(name) => Some(name),
        Sexp::Str(text) | Sexp::Word(text) => Some(text),
        Sexp::Form { .. } => None,
    }
}

/// Where a piece of the parsed text starts, for an error position.
fn sexp_position(sexp: &Sexp) -> usize {
    match sexp {
        Sexp::Form { position, .. } => *position,
        // Atoms do not record a position, and none of them can fail this way: every atom has text.
        _ => 0,
    }
}

/// A property key as a form accepts it: the leading `:` removed, surrounding whitespace trimmed.
///
/// Nothing else is done to it. Folding a key is `document::Property::normalized_key`'s rule, and a
/// second copy of it here would be a second version of the same rule — the parser hands over the
/// spelling a person wrote and the executor applies the one rule.
fn property_key(form: &str, written: &str) -> Result<String, QueryError> {
    let key = written.trim().strip_prefix(':').unwrap_or(written.trim());
    let key = key.trim();
    if key.is_empty() {
        return Err(QueryError::InvalidArgument {
            form: form.to_owned(),
            argument: written.to_owned(),
            detail: "expected a property key, such as `(property type)` or `(page-property \
                     :journal?)`"
                .to_owned(),
        });
    }
    Ok(key.to_owned())
}

/// One side of a `(between …)` interval.
fn date_operand(form: &str, written: &Sexp, text: &str) -> Result<DateOperand, QueryError> {
    // A reference is the absolute spelling: the graph is the only thing that can say which day a
    // journal page title denotes, so the name is kept for the executor to resolve.
    if let Sexp::PageRef(name) = written {
        return Ok(DateOperand::JournalPage(name.clone()));
    }
    let folded = text.trim().to_ascii_lowercase();
    let plain = match folded.as_str() {
        "today" | "now" => Some(DateOperand::Today),
        "yesterday" => Some(DateOperand::OffsetDays(-1)),
        "tomorrow" => Some(DateOperand::OffsetDays(1)),
        _ => None,
    };
    if let Some(operand) = plain {
        return Ok(operand);
    }
    let invalid = |detail: &str| QueryError::InvalidArgument {
        form: form.to_owned(),
        argument: text.to_owned(),
        detail: detail.to_owned(),
    };
    if folded.len() == 8 && folded.bytes().all(|byte| byte.is_ascii_digit()) {
        let stored = format!("{}-{}-{}", &folded[..4], &folded[4..6], &folded[6..]);
        return JournalDate::parse_stored(&stored)
            .map(DateOperand::Day)
            .ok_or_else(|| invalid("expected a valid journal day in YYYYMMDD form"));
    }
    let Some(sign) = folded.chars().next() else {
        return Err(invalid("expected a date operand"));
    };
    let sign = match sign {
        '+' => 1,
        '-' => -1,
        _ => return Err(invalid(
            "expected `today`, `now`, `yesterday`, `tomorrow`, `+3d`, `-2w`, `+1m`, `-1y`, or a \
                 journal page such as `[[2020-12-05]]`",
        )),
    };
    let rest = &folded[1..];
    let Some(unit) = rest.chars().last() else {
        return Err(invalid("expected a number and a unit after the sign"));
    };
    let digits = &rest[..rest.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(
            "expected a whole number of days, weeks, months, or years",
        ));
    }
    let count = digits
        .parse::<i64>()
        .map_err(|_| invalid("that offset is too large"))?;
    let scaled = |unit_in_days: i64| {
        count
            .checked_mul(unit_in_days)
            .and_then(|scaled| scaled.checked_mul(sign))
            .map(DateOperand::OffsetDays)
            .ok_or_else(|| invalid("that offset is too large"))
    };
    let scaled_months = |unit_in_months: i64| {
        count
            .checked_mul(unit_in_months)
            .and_then(|scaled| scaled.checked_mul(sign))
            .map(DateOperand::OffsetMonths)
            .ok_or_else(|| invalid("that offset is too large"))
    };
    match unit {
        'd' => scaled(1),
        'w' => scaled(7),
        'm' => scaled_months(1),
        'y' => scaled_months(12),
        _ => Err(invalid(
            "expected a unit: `d` days, `w` weeks, `m` months, or `y` years",
        )),
    }
}

/// A form's argument count is not one it takes.
fn arity(position: usize, name: &str, expected: &str, found: usize) -> QueryError {
    QueryError::Syntax {
        position,
        detail: format!("`({name} …)` takes {expected}, found {found}"),
    }
}

/// Join a list of words with commas and an Oxford-style "or": `["A", "B", "C"]` -> `"A, B, or C"`.
fn listed_or(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [head @ .., last] => format!("{}, or {last}", head.join(", ")),
    }
}
