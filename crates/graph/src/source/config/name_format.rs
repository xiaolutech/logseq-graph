use std::fmt;

use super::remove_empty_namespace_segments;

/// How Logseq maps a page name to a Markdown file name.
///
/// Only the format this project has real-graph evidence for is implemented. A graph that selects
/// another format is rejected instead of decoded approximately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum NameFormat {
    /// Logseq's default: `/` is written as `___` and other non-alphanumeric ASCII as `%XX`.
    #[default]
    TripleLowbar,
}

impl NameFormat {
    /// Decode a Markdown file stem into the page name Logseq shows.
    ///
    /// The order is Logseq's own (`tri-lb-title-parsing` and `make-valid-namespaces` in its graph
    /// parser), in three stages:
    ///
    /// 1. `___` becomes `/`.
    /// 2. The percent escapes are decoded.
    /// 3. The name is split on `/`, every empty segment is discarded, and the rest is joined with
    ///    `/`, so `a//b`, `/a`, and `a/` all read as `a/b`.
    ///
    /// The order is visible in a real graph: `A-mu%5F%5F%5F_.md` is the page `A-mu____`, because the
    /// three escaped underscores decode to `___` only after the `___` that would mean a namespace
    /// has already been replaced. Decoding percent escapes first would read that file as `A-mu/_`,
    /// a page no graph holds.
    ///
    /// Empty-segment removal comes after percent decoding for the same reason: an escaped separator
    /// such as `%2F` must already be a `/` before segments are counted, or `%2Fa` would keep an
    /// empty leading segment that Logseq does not display.
    pub(crate) fn decode(self, stem: &str) -> Result<String, super::NameDecodeError> {
        let name = match self {
            Self::TripleLowbar => stem.replace("___", "/"),
        };
        let name = percent_decode(&name)?;
        Ok(remove_empty_namespace_segments(&name))
    }

    /// Encode a page name into a Markdown file stem, or refuse rather than write a name that would
    /// decode to something else.
    ///
    /// The rules come from one real graph of 14136 files: `/` is written as `___`; `:`, `|`, `?`,
    /// `#`, `%`, `"`, and `\` are percent-encoded; a leading `.` is percent-encoded because a
    /// dot-prefixed file is never discovered as a page; a run of three underscores is percent-encoded
    /// because `___` decodes to `/`. Everything else stays literal, including spaces, non-ASCII
    /// text, `-`, `(`, `)`, `,`, and inner `.` and `_`, which is what the same graph writes.
    ///
    /// When a name holds a `/`, every literal underscore is also percent-encoded: the `___` that
    /// stands for the slash would otherwise swallow underscores the name itself holds.
    ///
    /// The returned stem is decoded again before it is returned. A name this encoder cannot
    /// represent is a [`NameEncodeError`] rather than a file whose
    /// name silently denotes another page. That includes non-canonical names with empty namespace
    /// segments such as `a//b`, `/a`, and `a/`: the decoder now removes empty segments like Logseq
    /// does, so their stem decodes to `a/b`, not to the requested name, and the self-check refuses
    /// them instead of writing a file Logseq displays under another name. Refusing is the same position this crate takes for
    /// `:org` graphs and other `:file/name-format` values: never approximate a name.
    pub(crate) fn encode(self, name: &str) -> Result<String, super::NameEncodeError> {
        match self {
            Self::TripleLowbar => encode_triple_lowbar(name),
        }
    }
}

/// Failure of file-name decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NameDecodeError {
    valid_up_to: usize,
}

impl fmt::Display for NameDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "percent escapes do not decode to UTF-8: invalid byte at offset {}",
            self.valid_up_to
        )
    }
}

/// Failure of file-name encoding: the page name has no stem that decodes back to it.
///
/// A name that fails here is refused instead of being written under a stem that decodes to a
/// different page, which would create a file nothing in the graph can find again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameEncodeError {
    name: String,
    reason: String,
}

impl NameEncodeError {
    fn new(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            reason: reason.into(),
        }
    }

    /// Why no stem decodes back to the name.
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for NameEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the page name {:?} has no file name that decodes back to it: {}",
            self.name, self.reason
        )
    }
}

pub(super) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Encode one page name for `:triple-lowbar`, then prove the result decodes back to it.
///
/// The page name is scanned once. When the name holds no `/`, a run of three underscores is written
/// as `%5F%5F%5F`, so the literal `___` a name holds cannot be mistaken for a namespace separator,
/// and shorter runs stay literal. When the name does hold a `/`, every literal underscore is
/// written as `%5F` instead: the `___` that the `/` itself becomes is the separator and must not be
/// escaped, and without escaping, the separators could swallow underscores the name holds. The
/// special characters are written as the escapes a real graph uses in both cases.
fn encode_triple_lowbar(name: &str) -> Result<String, NameEncodeError> {
    // `page_name` in `discovery` trims a decoded name, so a name with outer whitespace could never
    // be observed as written: the file would decode to a different page. `decode` alone does not
    // trim, which is why this is checked here instead of being left to the self-check below.
    if name.is_empty() {
        return Err(NameEncodeError::new(name, "the page name is empty"));
    }
    if name.trim() != name {
        return Err(NameEncodeError::new(
            name,
            "the page name starts or ends with whitespace, which discovery trims",
        ));
    }

    let mut encoded = String::with_capacity(name.len());
    // Escaping every literal underscore beside a separator costs nothing for writing and reads
    // back through the existing decode order (`___` first, percent escapes second), so the choice
    // is per name rather than per underscore.
    let escape_underscores = name.contains('/');
    let mut literal_underscores = 0;
    for (index, character) in name.char_indices() {
        if character == '_' {
            if escape_underscores {
                encoded.push_str("%5F");
            } else {
                literal_underscores += 1;
                if literal_underscores == 3 {
                    encoded.push_str("%5F%5F%5F");
                    literal_underscores = 0;
                }
            }
            continue;
        }
        for _ in 0..literal_underscores {
            encoded.push('_');
        }
        literal_underscores = 0;
        match character {
            '/' => encoded.push_str("___"),
            ':' => encoded.push_str("%3A"),
            '|' => encoded.push_str("%7C"),
            '?' => encoded.push_str("%3F"),
            '#' => encoded.push_str("%23"),
            '%' => encoded.push_str("%25"),
            '"' => encoded.push_str("%22"),
            '\\' => encoded.push_str("%5C"),
            // A dot-prefixed file is skipped by discovery as an internal or an editor leftover, so
            // the leading dot is the only position where `.` has to be escaped. Real graphs keep
            // every other dot literal.
            '.' if index == 0 => encoded.push_str("%2E"),
            other => encoded.push(other),
        }
    }
    for _ in 0..literal_underscores {
        encoded.push('_');
    }

    // The self-check is the point of this function: the rules above are evidence, not a proof, so
    // the stem is decoded again before it is handed out. A name the rules do not cover becomes a
    // refusal here instead of a file that decodes to a different page.
    match NameFormat::TripleLowbar.decode(&encoded) {
        Ok(decoded) if decoded == name => Ok(encoded),
        Ok(decoded) => Err(NameEncodeError::new(
            name,
            format!("the stem {encoded:?} decodes to {decoded:?}"),
        )),
        Err(error) => Err(NameEncodeError::new(
            name,
            format!("the stem {encoded:?} does not decode: {error}"),
        )),
    }
}

/// Decode `%XX` byte escapes, leaving every other byte as written.
fn percent_decode(input: &str) -> Result<String, NameDecodeError> {
    if !input.as_bytes().contains(&b'%') {
        return Ok(input.to_owned());
    }

    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(out).map_err(|error| NameDecodeError {
        valid_up_to: error.utf8_error().valid_up_to(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_escapes_decode_to_utf8() {
        assert_eq!(
            NameFormat::TripleLowbar.decode("New to Logseq%3F").unwrap(),
            "New to Logseq?"
        );
        assert_eq!(
            NameFormat::TripleLowbar
                .decode("%E6%B5%8B%E8%AF%95")
                .unwrap(),
            "测试"
        );
        assert_eq!(NameFormat::TripleLowbar.decode("50%").unwrap(), "50%");
    }

    #[test]
    fn percent_escapes_must_decode_to_utf8() {
        let error = NameFormat::TripleLowbar
            .decode("bad%E6")
            .expect_err("partial UTF-8 sequence is rejected");
        assert_eq!(error.valid_up_to, 3);
    }

    #[test]
    fn empty_namespace_segments_are_removed_like_logseq() {
        // Logseq's `make-valid-namespaces` splits the decoded name on `/`, drops every empty
        // segment, and joins with `/`. The cases span the stages: raw separators, leading and
        // trailing ones, and separators that only exist after percent decoding.
        for (stem, expected) in [
            ("a______b", "a/b"),
            ("___a", "a"),
            ("a___", "a"),
            ("%2Fa", "a"),
            ("a%2F", "a"),
            ("a%2F%2Fb", "a/b"),
            ("a___b", "a/b"),
        ] {
            assert_eq!(
                NameFormat::TripleLowbar.decode(stem).expect("decode"),
                expected,
                "{stem}"
            );
        }
    }

    #[test]
    fn names_with_empty_namespace_segments_are_refused() {
        // Each of these displays as `a/b` (or `a`) in Logseq, so writing a file for the name as
        // given would create a page nothing can find again under the requested name.
        for name in ["a//b", "/a", "a/", "/"] {
            let error = NameFormat::TripleLowbar.encode(name).expect_err("refused");
            assert!(
                error.to_string().contains(name),
                "the error names the requested name {name:?}: {error}"
            );
        }

        // The canonical spelling still round-trips.
        assert_eq!(
            NameFormat::TripleLowbar.encode("a/b").expect("encode"),
            "a___b"
        );
        assert_eq!(
            NameFormat::TripleLowbar.decode("a___b").expect("decode"),
            "a/b"
        );
    }

    #[test]
    fn ordinary_page_names_encode_unchanged() {
        for name in ["Basic settings", "0xProto", "QUERY", "C++ 入门"] {
            assert_eq!(
                NameFormat::TripleLowbar.encode(name).expect("encode"),
                name,
                "{name}"
            );
        }
    }

    #[test]
    fn a_namespace_separator_encodes_to_triple_lowbar() {
        let encoded = NameFormat::TripleLowbar
            .encode("长安/启源")
            .expect("encode a namespace");
        assert_eq!(encoded, "长安___启源");
        assert_eq!(
            NameFormat::TripleLowbar.decode(&encoded).expect("decode"),
            "长安/启源"
        );
    }

    #[test]
    fn spaces_stay_literal() {
        let encoded = NameFormat::TripleLowbar
            .encode("New to Logseq")
            .expect("encode");
        assert_eq!(encoded, "New to Logseq");
        assert!(
            !encoded.contains("%20"),
            "a space is literal, not an escape: {encoded}"
        );
    }

    #[test]
    fn an_underscore_next_to_a_slash_encodes_and_round_trips() {
        // With a slash in the name, the `___` that stands for the slash would swallow one of the
        // name's own underscores when decoded. Every literal underscore is escaped instead, so the
        // separator is the only `___` in the stem and the decode order (separators first, percent
        // escapes second) gives the name back.
        for (name, stem) in [
            ("a_/b", "a%5F___b"),
            ("a/_b", "a___%5Fb"),
            ("a_b/c_d", "a%5Fb___c%5Fd"),
        ] {
            let encoded = NameFormat::TripleLowbar.encode(name).expect("encode");
            assert_eq!(encoded, stem);
            assert_eq!(
                NameFormat::TripleLowbar.decode(&encoded).expect("decode"),
                name
            );
        }
    }

    #[test]
    fn underscores_stay_literal_when_the_name_holds_no_slash() {
        assert_eq!(
            NameFormat::TripleLowbar.encode("a__b").expect("encode"),
            "a__b"
        );
    }

    #[test]
    fn underscores_in_runs_of_three_encode_to_percent_escapes() {
        // The real graph spells the page `A-mu____` as `A-mu%5F%5F%5F_`: the first three underscores
        // would otherwise read as a namespace separator.
        assert_eq!(
            NameFormat::TripleLowbar.encode("A-mu____").expect("encode"),
            "A-mu%5F%5F%5F_"
        );
        assert_eq!(
            NameFormat::TripleLowbar.encode("a__b").expect("encode"),
            "a__b"
        );
    }

    #[test]
    fn a_leading_dot_encodes_so_the_file_is_not_hidden() {
        assert_eq!(
            NameFormat::TripleLowbar.encode(".zshrc").expect("encode"),
            "%2Ezshrc"
        );
        assert_eq!(
            NameFormat::TripleLowbar.encode("1cat.fun").expect("encode"),
            "1cat.fun"
        );
    }

    #[test]
    fn the_real_graph_names_round_trip_both_ways() {
        // Every line is one file taken from a real graph of 14136 files: the page name Logseq shows,
        // and the file stem it lives in. Encoding has to reproduce that exact stem, and decoding has
        // to give the name back, or one direction of this slice disagrees with the graph it serves.
        //
        // The escapes covered here are `%3A`, `%7C`, `%3F`, `%23`, `%25`, `%2E`, `%22`, `%5F`,
        // `%5C`, and `___`, plus literal spaces, literal non-ASCII text, a literal inner `.`, a
        // literal single `_`, and a name that needs no escape at all.
        const FIXTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/page-names.tsv"
        ));

        let mut lines = 0;
        for (index, line) in FIXTURE.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let (name, stem) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("line {} is not name<TAB>file: {line:?}", index + 1));
            assert_eq!(
                NameFormat::TripleLowbar.encode(name).expect("encode"),
                stem,
                "line {} encodes {name:?} to a different stem",
                index + 1
            );
            assert_eq!(
                NameFormat::TripleLowbar.decode(stem).expect("decode"),
                name,
                "line {} decodes {stem:?} to a different name",
                index + 1
            );
            lines += 1;
        }
        assert!(lines >= 18, "the fixture lost rows: {lines}");
    }
}
