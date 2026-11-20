//! Graph configuration: the keys that affect Markdown page discovery, and the `:favorites` list.
//!
//! Logseq keeps graph configuration in `logseq/config.edn`, an EDN map. This crate does not take
//! an EDN dependency: the module reads only the top-level keys it can honor, and it skips
//! everything else without interpreting it. Values it cannot honor are errors, because guessing
//! would silently disagree with Logseq.
//!
//! Honoured keys: `:file/name-format`, `:journal/file-name-format`, `:journal/page-title-format`,
//! `:preferred-format`, `:text-formats` (empty only), and `:favorites`.
//!
//! `:favorites` is the one honoured key discovery never consults. It lists the page names a person
//! pinned in Logseq's sidebar, so it is a graph fact a reader displays rather than a rule that
//! decides which file is which page. It is read here because it lives in this map and this scanner
//! already reads it; the list is exposed exactly as written, in written order. A name whose page no
//! longer exists is kept: the entry is the person's configuration, and a deleted or renamed page is
//! a state of that configuration rather than a typo in it. Filtering such a name out here would
//! edit the list to match the current graph; a caller that needs to know which entries still have a
//! page asks the read surface, which answers that fact beside the names.
//!
//! `:file/name-format` decodes regular page file names only. A journal is named by a date, so its
//! two keys are separate and are honoured by [`super::journal`]: `:journal/file-name-format` reads
//! the date out of `journals/`, and `:journal/page-title-format` renders the title Logseq shows.
//! Both accept only the pattern subset documented there; any other pattern is an error rather than
//! an approximation. They are date patterns, not enumerations, so they are read as strings only:
//! Logseq writes them as strings, and accepting a keyword there would turn a typo into a silent
//! fallback.
//!
//! `:preferred-format` is read from either an EDN keyword or a string. Logseq's own `config.edn`
//! template writes `:markdown`, so refusing the keyword shape would refuse a default graph;
//! Markdown is the only format this crate parses, so any other value is still an error.
//!
//! Deliberately ignored keys that could otherwise change interpretation:
//!
//! - `:hidden` lists paths under the graph root. Discovery never scans the graph root, only
//!   `pages/` and `journals/`, so a hidden root file is already out of reach.
//! - `:feature/enable-journals?` is a Logseq UI switch. Journal files live in `journals/` whether
//!   the switch is on or off, so discovery follows the directory.
//! - `:default-templates` holds the content of new pages, which is not a discovery input.
//! - `:preferred-language` selects the interface language. Journal titles are rendered with the
//!   fixed English month and weekday names, which is what Logseq's default pattern produces, so the
//!   key does not change a discovered name.
//! - Every other key is presentation, feature flag, or content configuration.

use std::path::Path;

use super::error::SourceError;
use super::file;
use super::journal::{JournalFileNameFormat, JournalPageTitleFormat};

/// Graph-relative path of the Logseq configuration file.
const CONFIG_PATH: &str = "logseq/config.edn";

/// Logseq's default `:journal/file-name-format`.
const DEFAULT_JOURNAL_FILE_NAME_FORMAT: &str = "yyyy_MM_dd";

/// Logseq's default `:journal/page-title-format`.
const DEFAULT_JOURNAL_PAGE_TITLE_FORMAT: &str = "MMM do, yyyy";

mod edn;
mod name_format;

use edn::{Reader, Value};
pub(crate) use name_format::{NameDecodeError, NameEncodeError, NameFormat};

/// The configuration this build honors when discovering pages.
///
/// A graph without `logseq/config.edn` gets Logseq's defaults, which is also what an empty file or
/// a file that starts with a byte order mark means. Directories and hidden entries follow fixed
/// rules in the `discovery` module; the keys this module ignores on purpose are listed in the
/// module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphConfig {
    name_format: NameFormat,
    journal_file_name_format: JournalFileNameFormat,
    journal_page_title_format: JournalPageTitleFormat,
    favorites: Vec<String>,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            name_format: NameFormat::default(),
            #[expect(
                clippy::expect_used,
                reason = "`yyyy_MM_dd` names exactly one year, one padded month, and one padded day \
                          with no two variable-width fields adjacent, so `parse` accepts it; \
                          `journal_formats_default_to_logseq_patterns` pins this default against \
                          the parsed literal"
            )]
            journal_file_name_format: JournalFileNameFormat::parse(
                DEFAULT_JOURNAL_FILE_NAME_FORMAT,
            )
            .expect("the default journal file name format is supported"),
            #[expect(
                clippy::expect_used,
                reason = "`MMM do, yyyy` is the pattern Logseq's default config states and the \
                          parser's accepted subset covers every token in it; \
                          `journal_formats_default_to_logseq_patterns` pins this default against \
                          the parsed literal"
            )]
            journal_page_title_format: JournalPageTitleFormat::parse(
                DEFAULT_JOURNAL_PAGE_TITLE_FORMAT,
            )
            .expect("the default journal page title format is supported"),
            favorites: Vec::new(),
        }
    }
}

impl GraphConfig {
    pub(crate) fn signature(&self) -> String {
        let mut signature = match self.name_format {
            NameFormat::TripleLowbar => String::from("v1;triple-lowbar;"),
        };
        for value in [
            self.journal_file_name_format.pattern(),
            self.journal_page_title_format.pattern(),
        ]
        .into_iter()
        .chain(self.favorites.iter().map(String::as_str))
        {
            signature.push_str(&value.len().to_string());
            signature.push(':');
            signature.push_str(value);
        }
        signature
    }

    /// How page names are encoded in file names.
    pub(crate) fn name_format(&self) -> NameFormat {
        self.name_format
    }

    /// How a journal file name under `journals/` spells its date.
    pub(crate) fn journal_file_name_format(&self) -> &JournalFileNameFormat {
        &self.journal_file_name_format
    }

    /// How a journal's date is rendered into the title Logseq displays.
    pub(crate) fn journal_page_title_format(&self) -> &JournalPageTitleFormat {
        &self.journal_page_title_format
    }

    /// Page names `:favorites` lists, in written order, including names no page holds.
    pub(crate) fn favorites(&self) -> &[String] {
        &self.favorites
    }

    /// Load the configuration of the graph rooted at `root`.
    pub(crate) fn load(root: &Path) -> Result<Self, SourceError> {
        let path = root.join(CONFIG_PATH);
        match file::read_text_if_exists(&path)? {
            Some(text) => Self::parse(&text, &path),
            None => Ok(Self::default()),
        }
    }

    /// Parse the supported subset of an EDN configuration map.
    pub(crate) fn parse(text: &str, path: &Path) -> Result<Self, SourceError> {
        // A leading byte order mark is an editor artifact. Page files keep theirs because rendering
        // must reproduce them; the configuration is only read, so one leading mark is skipped.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut reader = Reader::new(text);
        let mut config = Self::default();

        reader.skip_trivia();
        if reader.at_end() {
            return Ok(config);
        }
        if reader.peek() != Some(b'{') {
            return Err(invalid(path, "expected a map at the top level"));
        }
        reader.bump();

        loop {
            reader.skip_trivia();
            match reader.peek() {
                None => return Err(invalid(path, "top-level map is not closed")),
                Some(b'}') => {
                    reader.bump();
                    break;
                }
                Some(b':') => {}
                Some(other) => {
                    return Err(invalid(
                        path,
                        &format!("expected a keyword, found `{}`", other as char),
                    ));
                }
            }

            let key = reader.read_keyword();
            reader.skip_trivia();
            let value = reader
                .read_value()
                .map_err(|detail| invalid(path, &detail))?;
            config.apply(&key, value, path)?;
        }

        reader.skip_trivia();
        if !reader.at_end() {
            return Err(invalid(path, "content follows the top-level map"));
        }
        Ok(config)
    }

    /// Apply one top-level configuration entry.
    fn apply(&mut self, key: &str, value: Value, path: &Path) -> Result<(), SourceError> {
        match (key, value) {
            ("file/name-format", Value::Keyword(name)) => {
                self.name_format = match name.as_str() {
                    "triple-lowbar" => NameFormat::TripleLowbar,
                    unsupported => {
                        return Err(SourceError::UnsupportedConfig {
                            path: path.to_owned(),
                            key: ":file/name-format",
                            value: format!(":{unsupported}"),
                        });
                    }
                };
            }
            ("file/name-format", _) => {
                return Err(invalid(path, ":file/name-format must be a keyword"));
            }
            ("journal/file-name-format", Value::Text(pattern)) => {
                self.journal_file_name_format =
                    JournalFileNameFormat::parse(&pattern).map_err(|_| {
                        SourceError::UnsupportedConfig {
                            path: path.to_owned(),
                            key: ":journal/file-name-format",
                            value: pattern.clone(),
                        }
                    })?;
            }
            ("journal/file-name-format", _) => {
                return Err(invalid(path, ":journal/file-name-format must be a string"));
            }
            ("journal/page-title-format", Value::Text(pattern)) => {
                self.journal_page_title_format =
                    JournalPageTitleFormat::parse(&pattern).map_err(|_| {
                        SourceError::UnsupportedConfig {
                            path: path.to_owned(),
                            key: ":journal/page-title-format",
                            value: pattern.clone(),
                        }
                    })?;
            }
            ("journal/page-title-format", _) => {
                return Err(invalid(path, ":journal/page-title-format must be a string"));
            }
            // Logseq writes the keyword `:markdown` in its own `config.edn` template and accepts
            // the string form when a person writes it, so both shapes name the same format.
            ("preferred-format", Value::Keyword(format)) => {
                if !is_markdown(&format) {
                    return Err(SourceError::UnsupportedConfig {
                        path: path.to_owned(),
                        key: ":preferred-format",
                        value: format!(":{format}"),
                    });
                }
            }
            ("preferred-format", Value::Text(format)) => {
                if !is_markdown(&format) {
                    return Err(SourceError::UnsupportedConfig {
                        path: path.to_owned(),
                        key: ":preferred-format",
                        value: format,
                    });
                }
            }
            ("preferred-format", _) => {
                return Err(invalid(
                    path,
                    ":preferred-format must be a keyword or a string",
                ));
            }
            // An empty list is Logseq's default and declares nothing extra. A non-empty list
            // declares additional text formats, which this Markdown-only build cannot honor.
            ("text-formats", Value::Collection { source, empty, .. }) => {
                if !empty {
                    return Err(SourceError::UnsupportedConfig {
                        path: path.to_owned(),
                        key: ":text-formats",
                        value: source,
                    });
                }
            }
            ("text-formats", _) => {
                return Err(invalid(path, ":text-formats must be a collection"));
            }
            // Kept as written, including a name no page holds: see the module documentation. An
            // element that is not a string is a shape this module cannot honor, so it is refused
            // rather than skipped, exactly like every other honoured key.
            (
                "favorites",
                Value::Collection {
                    strings: Some(names),
                    ..
                },
            ) => {
                self.favorites = names;
            }
            ("favorites", _) => {
                return Err(invalid(path, ":favorites must be a vector of strings"));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Whether a `:preferred-format` name selects the grammar this crate parses.
///
/// Logseq accepts Markdown or Org. This slice parses Markdown only, so an Org graph is refused
/// instead of being read with the wrong grammar. An empty string means "use the global
/// preference" in Logseq, which is also not a format we can honor.
fn is_markdown(name: &str) -> bool {
    name.eq_ignore_ascii_case("markdown")
}

/// Discard empty `/`-separated segments, as Logseq's `make-valid-namespaces` does.
///
/// `a//b`, `/a`, and `a/` all display as `a/b` in Logseq, so a file whose name decodes to a name
/// with empty segments must be read as the name without them.
fn remove_empty_namespace_segments(name: &str) -> String {
    let segments: Vec<&str> = name
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    segments.join("/")
}

fn invalid(path: &Path, detail: &str) -> SourceError {
    SourceError::InvalidConfig {
        path: path.to_owned(),
        detail: detail.to_owned(),
    }
}

/// The `:journal/page-title-format` the graph rooted at `root` states, or Logseq's default.
///
/// The served RPC answers this pattern as `logseq.App.getUserConfigs`'s `preferredDateFormat`; a
/// consumer passes it to its date formatter to name a journal page, so the pattern text is the fact
/// to state, not a parsed form. A graph whose configuration cannot be read or parsed answers the
/// default rather than a pattern the graph does not state; that failure is already published by the
/// scan, and this read is not the place to report it twice.
pub fn journal_page_title_format(root: &Path) -> String {
    GraphConfig::load(root)
        .map(|config| config.journal_page_title_format().pattern().to_owned())
        .unwrap_or_else(|_| DEFAULT_JOURNAL_PAGE_TITLE_FORMAT.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("graph/logseq/config.edn")
    }

    fn parse(text: &str) -> Result<GraphConfig, SourceError> {
        GraphConfig::parse(text, &path())
    }

    #[test]
    fn signatures_encode_supported_configuration_unambiguously() {
        let defaults = GraphConfig::default().signature();
        assert_eq!(defaults, parse("{}").expect("empty config").signature());
        assert_eq!(
            defaults,
            parse("{:unknown true}").expect("ignored key").signature()
        );
        for text in [
            r#"{:journal/file-name-format "yyyy-MM-dd"}"#,
            r#"{:journal/page-title-format "yyyy-MM-dd"}"#,
            r#"{:favorites ["中文"]}"#,
        ] {
            assert_ne!(defaults, parse(text).expect("changed config").signature());
        }
        assert_ne!(
            parse(r#"{:favorites ["ab" "c"]}"#)
                .expect("favorites")
                .signature(),
            parse(r#"{:favorites ["a" "bc"]}"#)
                .expect("favorites")
                .signature(),
        );
    }

    #[test]
    fn missing_name_format_uses_triple_lowbar() {
        let config = parse("{:preferred-format \"Markdown\"}").expect("parse config");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
    }

    #[test]
    fn empty_config_file_uses_defaults() {
        assert_eq!(
            parse("   \n; only a comment\n").expect("parse"),
            GraphConfig::default()
        );
    }

    #[test]
    fn unrelated_keys_and_nested_values_are_skipped() {
        let text = r#"
            {:text-formats []
             :hidden ["script" "README.md"]
             :default-templates {:journals ""}
             :macros {"poem" "Rose is $1, violet's $2"}
             :feature/enable-journals? false
             :preferred-format "Markdown"
             :file/name-format :triple-lowbar}
        "#;
        let config = parse(text).expect("parse config");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
    }

    #[test]
    fn commented_out_key_is_not_read() {
        let text = "{; :file/name-format :percent-encoding\n :file/name-format :triple-lowbar}";
        let config = parse(text).expect("parse config");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
    }

    #[test]
    fn nested_name_format_does_not_apply() {
        let text = "{:default-templates {:file/name-format :percent-encoding}}";
        let config = parse(text).expect("nested key is ignored");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
    }

    #[test]
    fn unsupported_name_format_is_rejected() {
        let error = parse("{:file/name-format :percent-encoding}").expect_err("rejected");
        match error {
            SourceError::UnsupportedConfig { key, value, .. } => {
                assert_eq!(key, ":file/name-format");
                assert_eq!(value, ":percent-encoding");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn journal_formats_default_to_logseq_patterns() {
        let config = parse("{:preferred-format \"Markdown\"}").expect("parse config");
        assert_eq!(
            config.journal_file_name_format(),
            &JournalFileNameFormat::parse("yyyy_MM_dd").expect("default file name format")
        );
        assert_eq!(
            config.journal_page_title_format(),
            &JournalPageTitleFormat::parse("MMM do, yyyy").expect("default page title format")
        );
    }

    #[test]
    fn custom_journal_formats_are_read() {
        let config = parse(
            r#"{:journal/file-name-format "yyyy-MM-dd" :journal/page-title-format "yyyy-MM-dd EEEE"}"#,
        )
        .expect("parse config");
        assert_eq!(
            config.journal_file_name_format(),
            &JournalFileNameFormat::parse("yyyy-MM-dd").expect("file name format")
        );
        assert_eq!(
            config.journal_page_title_format(),
            &JournalPageTitleFormat::parse("yyyy-MM-dd EEEE").expect("page title format")
        );
    }

    #[test]
    fn unsupported_journal_formats_are_rejected() {
        for (text, expected) in [
            (
                r#"{:journal/file-name-format "yy_MM_dd"}"#,
                ":journal/file-name-format",
            ),
            (
                r#"{:journal/page-title-format "yyyy-MM-dd HH"}"#,
                ":journal/page-title-format",
            ),
        ] {
            let error = parse(text).expect_err("unsupported pattern is rejected");
            match error {
                SourceError::UnsupportedConfig { key, .. } => assert_eq!(key, expected),
                other => panic!("unexpected error for {text}: {other}"),
            }
        }
    }

    #[test]
    fn journal_formats_must_be_strings() {
        // These are date patterns, not enumerations. Logseq writes them as strings, so a keyword
        // is a typo, not an equivalent spelling: widening this key would hide the typo.
        for text in [
            "{:journal/file-name-format :yyyy_MM_dd}",
            "{:journal/file-name-format nil}",
            "{:journal/page-title-format :yyyy_MM_dd}",
            "{:journal/page-title-format nil}",
        ] {
            let error = parse(text).expect_err("non-string pattern is rejected");
            match error {
                SourceError::InvalidConfig { detail, .. } => assert!(
                    detail.contains("must be a string"),
                    "unexpected detail for {text}: {detail}"
                ),
                other => panic!("unexpected error for {text}: {other}"),
            }
        }
    }

    #[test]
    fn keyword_preferred_format_uses_the_default() {
        let config = parse("{:preferred-format :markdown}").expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn unsupported_keyword_preferred_format_is_rejected() {
        let error = parse("{:preferred-format :org}").expect_err("rejected");
        match error {
            SourceError::UnsupportedConfig { key, value, .. } => {
                assert_eq!(key, ":preferred-format");
                assert_eq!(value, ":org");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn string_preferred_format_is_still_accepted() {
        let config = parse("{:preferred-format \"markdown\"}").expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn preferred_format_is_case_insensitive() {
        let config = parse("{:preferred-format \"Markdown\"}").expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn preferred_format_rejects_other_shapes_and_names_both() {
        let error = parse("{:preferred-format 42}").expect_err("rejected");
        match error {
            SourceError::InvalidConfig { detail, .. } => {
                assert!(detail.contains("keyword"), "no keyword in: {detail}");
                assert!(detail.contains("string"), "no string in: {detail}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn keyword_preferred_format_coexists_with_name_format() {
        let config = parse("{:preferred-format :markdown, :file/name-format :triple-lowbar}")
            .expect("parse config");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn unsupported_preferred_format_is_rejected() {
        let error = parse("{:preferred-format \"Org\"}").expect_err("rejected");
        match error {
            SourceError::UnsupportedConfig { key, value, .. } => {
                assert_eq!(key, ":preferred-format");
                assert_eq!(value, "Org");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn unclosed_map_is_rejected() {
        let error = parse("{:file/name-format :triple-lowbar").expect_err("rejected");
        assert!(matches!(error, SourceError::InvalidConfig { .. }));
    }

    #[test]
    fn trailing_content_is_rejected() {
        let error = parse("{:file/name-format :triple-lowbar} :extra").expect_err("rejected");
        assert!(matches!(error, SourceError::InvalidConfig { .. }));
    }

    #[test]
    fn escaped_string_value_is_decoded() {
        let config = parse(r#"{:preferred-format "Mark\u0064own"}"#).expect("parse");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
    }

    #[test]
    fn leading_byte_order_mark_is_accepted() {
        let config = parse("\u{feff}{:file/name-format :triple-lowbar}").expect("parse");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
    }

    #[test]
    fn empty_text_formats_is_accepted() {
        let config = parse("{:text-formats [] :file/name-format :triple-lowbar}").expect("parse");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
    }

    #[test]
    fn non_empty_text_formats_is_rejected() {
        let error = parse("{:text-formats [\"Org\"]}").expect_err("rejected");
        match error {
            SourceError::UnsupportedConfig { key, value, .. } => {
                assert_eq!(key, ":text-formats");
                assert_eq!(value, "[\"Org\"]");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn text_formats_must_be_a_collection() {
        let error = parse("{:text-formats nil}").expect_err("rejected");
        assert!(matches!(error, SourceError::InvalidConfig { .. }));
    }

    #[test]
    fn the_real_graph_favorites_list_is_read_in_written_order() {
        // One line of the reference graph's `logseq/config.edn`. EDN separates elements with
        // whitespace, so a name holds spaces, a colon, and Chinese text without being quoted
        // further, and the list is ordered by the person who wrote it.
        let text = r#"{:favorites ["l1:人生okr" "长期目标" "adr" "inbox" "focusing now" "on this day" "archived" "focus_now" "github stars" "todo" "av 影片" "my github stars" "gtd metrics" "how to remember what you learn" "lessonilearned" "random-thoughts"]}"#;
        let config = parse(text).expect("parse the real favorites line");
        assert_eq!(
            config.favorites(),
            [
                "l1:人生okr",
                "长期目标",
                "adr",
                "inbox",
                "focusing now",
                "on this day",
                "archived",
                "focus_now",
                "github stars",
                "todo",
                "av 影片",
                "my github stars",
                "gtd metrics",
                "how to remember what you learn",
                "lessonilearned",
                "random-thoughts",
            ]
        );
    }

    #[test]
    fn an_empty_favorites_list_reads_as_no_names() {
        let config = parse("{:favorites []}").expect("an empty list is legal");
        assert!(config.favorites().is_empty());
    }

    #[test]
    fn a_missing_favorites_key_reads_as_no_names() {
        let config = parse("{:preferred-format :markdown}").expect("parse config");
        assert!(config.favorites().is_empty());
    }

    #[test]
    fn a_favorite_name_keeps_escapes_decoded() {
        // A name is a page name, so it may hold a quote, a backslash, or a long dash written as an
        // escape. The list holds what Logseq would compare against a page name, not the source text.
        let text = r#"{:favorites ["say \"hi\"" "a\\b" "dash\u2014dash"]}"#;
        let config = parse(text).expect("parse escaped names");
        assert_eq!(
            config.favorites(),
            ["say \"hi\"", "a\\b", "dash\u{2014}dash"]
        );
    }

    #[test]
    fn a_list_of_strings_reads_like_a_vector() {
        // EDN's list and vector are different literals for the same ordered sequence. Refusing one
        // would reject a configuration this module can honor.
        let config = parse("{:favorites (\"a\" \"b\")}").expect("parse a list");
        assert_eq!(config.favorites(), ["a", "b"]);
    }

    #[test]
    fn favorites_that_are_not_an_ordered_list_of_strings_are_rejected() {
        // Every rejected shape would otherwise answer a list that is not the one written: a
        // non-string element has no page name, a string is not a list, and a set loses the order
        // the person wrote.
        for text in [
            "{:favorites [\"a\" b]}",
            "{:favorites [1]}",
            "{:favorites \"a\"}",
            "{:favorites :a}",
            "{:favorites nil}",
            "{:favorites #{}}",
            "{:favorites {}}",
        ] {
            let error = parse(text).expect_err("not an ordered list of strings");
            match error {
                SourceError::InvalidConfig { detail, .. } => assert!(
                    detail.contains(":favorites"),
                    "unexpected detail for {text}: {detail}"
                ),
                other => panic!("unexpected error for {text}: {other}"),
            }
        }
    }

    #[test]
    fn logseq_default_config_shape_is_read() {
        let text = r#"
            {:preferred-format :markdown
             :hidden ["script" "README.md" "assets"]
             :default-templates {:journals "daily"}
             :journal/page-title-format "yyyy-MM-dd EEEE"
             :file/name-format :triple-lowbar
             :block-hidden-properties #{:lt-id}
             :property-pages/excludelist #{:type :tags :last}
             :property/separated-by-commas #{:authors :using :actors :director :artists}}
        "#;
        let config = parse(text).expect("parse the default template");
        assert_eq!(config.name_format(), NameFormat::TripleLowbar);
        assert_eq!(
            config.journal_page_title_format(),
            &JournalPageTitleFormat::parse("yyyy-MM-dd EEEE").expect("page title format")
        );
        assert_eq!(
            config.journal_file_name_format(),
            &JournalFileNameFormat::parse("yyyy_MM_dd").expect("default file name format")
        );
    }

    #[test]
    fn non_empty_text_formats_set_is_rejected() {
        // The set shape reaches the same rule as a vector: declaring formats is unsupported, and
        // the diagnostic keeps the literal as written, `#` included.
        let error = parse("{:text-formats #{[\"Org\"]}}").expect_err("rejected");
        match error {
            SourceError::UnsupportedConfig { key, value, .. } => {
                assert_eq!(key, ":text-formats");
                assert_eq!(value, "#{[\"Org\"]}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn empty_text_formats_set_is_accepted() {
        // An empty set declares no additional format, which is the same claim as `[]`.
        let config = parse("{:text-formats #{}}").expect("parse");
        assert_eq!(config, GraphConfig::default());
    }

    #[test]
    fn key_without_a_value_is_rejected() {
        for text in ["{:}", "{:hidden}", "{:hidden ,}"] {
            let error = parse(text).expect_err("missing value is rejected");
            assert!(
                matches!(error, SourceError::InvalidConfig { .. }),
                "unexpected error for {text}: {error}"
            );
        }
    }

    #[test]
    fn unsupported_values_are_quoted_in_the_diagnostic() {
        let error = parse("{:file/name-format :percent-encoding}").expect_err("rejected");
        assert!(
            error.to_string().contains("`:percent-encoding`"),
            "unquoted value in: {error}"
        );

        let error = parse("{:preferred-format \"\"}").expect_err("rejected");
        assert!(
            error.to_string().contains("to \"\""),
            "empty value is not visible in: {error}"
        );
    }
}
