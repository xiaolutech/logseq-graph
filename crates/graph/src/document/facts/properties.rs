use super::{PageDocument, VisibleLines};

/// Property key that carries a block's durable identity (ADR-0005).
///
/// One spelling for the whole crate: this is the key a block states its identity under, so the
/// projection's `blocks.durable_id` column, the snapshot's `durable_id` field, and the edits that
/// preserve an `id::` line all have to agree about it.
pub(in crate::document) const ID_PROPERTY: &str = "id";

/// One `key:: value` line.
///
/// A property line is any line of a page preamble or of a block's own text whose text before the
/// first `::` is a usable key. Logseq's editor writes properties at the top of a block, but the
/// text is scanned in full: the position is recorded, not enforced, because a property line after
/// content still states a fact and the block text keeps every line either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    line: usize,
    key: String,
    normalized_key: String,
    value: String,
}

impl Property {
    fn new(line: usize, key: &str, value: &str) -> Self {
        Self {
            line,
            key: key.to_owned(),
            normalized_key: normalize_key(key),
            value: value.to_owned(),
        }
    }

    /// Index of the source line the property was written on, into
    /// [`PageDocument::lines`](crate::document::PageDocument::lines).
    pub fn line(&self) -> usize {
        self.line
    }

    /// Key as written, without surrounding whitespace.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Key used for matching: lowercased with `_` replaced by `-`.
    ///
    /// This is the naming rule Logseq documents for properties, so `Done_At::` and `done-at::`
    /// name the same property. The written spelling stays available through [`Property::key`]
    /// because the source text owns it.
    ///
    /// Deliberately not the page-name rule
    /// ([`page_name_key`](crate::domain::page_name_key)): Logseq renames underscores in a property
    /// key and never in a page name, so `Done_At` and `Done At` are one property under two
    /// spellings only if the second were written `Done-At`. Sharing one function would quietly make
    /// a page name match a property name it does not match.
    pub fn normalized_key(&self) -> &str {
        &self.normalized_key
    }

    /// Value as written, without surrounding whitespace and with no newline in it.
    pub fn value(&self) -> &str {
        &self.value
    }
}
/// Property lines among `lines`, in source order, with each property carrying the absolute line
/// index supplied by the caller.
pub(super) fn extract_properties<'a>(
    lines: impl Iterator<Item = (usize, &'a str)>,
) -> Vec<Property> {
    VisibleLines::new(lines)
        .filter_map(|(line, text)| {
            parse_property_line(text).map(|(key, value)| Property::new(line, key, value))
        })
        .collect()
}

pub(super) fn extract_page_properties(document: &PageDocument) -> Vec<Property> {
    let preamble = document.preamble();
    let lines = document.raw_lines(preamble);
    if lines
        .first()
        .is_some_and(|line| line.text().trim() == "---")
    {
        if let Some(end) = lines[1..]
            .iter()
            .position(|line| line.text().trim() == "---")
        {
            return lines[1..=end]
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    let (key, value) = line.text().split_once(':')?;
                    let key = key.trim();
                    (!key.is_empty() && !key.contains(char::is_whitespace))
                        .then(|| Property::new(index + 1, key, value.trim()))
                })
                .collect();
        }
    }
    let mut properties = extract_properties(
        lines
            .iter()
            .enumerate()
            .map(|(offset, line)| (preamble.start() + offset, line.text())),
    );
    if let Some(first) = properties.first() {
        let start = first.line;
        let group_len = properties
            .iter()
            .enumerate()
            .take_while(|(offset, property)| property.line == start + offset)
            .count();
        properties.truncate(group_len);
    }
    properties
}

/// Return the last property whose normalized key matches `key`.
///
/// A repeated key is kept in the list at every occurrence, because the file states it more than
/// once; matching takes the last one, which is what a map built by insertion holds.
pub(super) fn lookup<'a>(properties: &'a [Property], key: &str) -> Option<&'a Property> {
    let wanted = normalize_key(key.trim());
    properties
        .iter()
        .rev()
        .find(|property| property.normalized_key == wanted)
}

/// Split one line into a property key and value.
///
/// The key is the text before the first `::`, trimmed. A line is not a property line when that text
/// is empty, starts with an ASCII digit, or contains whitespace, a control character, `:`, `[`,
/// `]`, or a backtick. Logseq's documented name rule is narrower (a non-digit first character, then
/// letters, digits, `. * + ! - _ ? $ % & = < >`); rejecting only the characters that make a prose
/// line look like a property keeps non-ASCII keys such as `股票代码:: 002565` working.
pub(super) fn parse_property_line(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once("::")?;
    let key = key.trim();
    let mut chars = key.chars();
    let first = chars.next()?;
    if first.is_ascii_digit()
        || key
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ':' | '[' | ']' | '`'))
    {
        return None;
    }
    Some((key, value.trim()))
}

/// The property-key rule, in the one place it is defined: lowercase, `_` folded to `-`.
///
/// It is exposed to the crate because the query executor compares a key a query wrote against the
/// key the projection stored, and the projection stored this rule's answer. A second copy of the
/// rule in the executor would be a second version of it — the failure mode this repository has
/// already paid for once with page names (`domain::page_name_key`). Deliberately not
/// `page_name_key`: Logseq renames underscores in a property key and never in a page name.
pub(crate) fn normalized_property_key(key: &str) -> String {
    normalize_key(key)
}

/// Lowercase a key and replace `_` with `-`.
fn normalize_key(key: &str) -> String {
    key.chars()
        .map(|c| match c {
            '_' => '-',
            other => other.to_lowercase().next().unwrap_or(other),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{parse, PageFacts};
    fn facts(source: &str) -> PageFacts {
        PageFacts::extract(&parse(source))
    }

    fn keys(properties: &[Property]) -> Vec<String> {
        properties
            .iter()
            .map(|property| property.key().to_owned())
            .collect()
    }

    #[test]
    fn page_properties_come_from_the_preamble_and_blocks_from_their_own_text() {
        let facts = facts(
            "title:: 首页\nDone_At:: 2026-10-29\n\n- 正文\n  id:: 11111111-2222-3333-4444-555555555555\n  collapsed:: true\n",
        );

        assert_eq!(keys(facts.properties()), vec!["title", "Done_At"]);
        assert_eq!(facts.properties()[0].value(), "首页");
        assert_eq!(facts.properties()[0].line(), 0);
        assert_eq!(facts.properties()[1].line(), 1);
        // Logseq lowercases property names and renames `_` to `-`.
        assert_eq!(facts.properties()[1].normalized_key(), "done-at");
        assert_eq!(
            facts
                .property("DONE-AT")
                .map(|property| property.value().to_owned()),
            Some("2026-10-29".to_owned())
        );

        let block = &facts.blocks()[0];
        assert_eq!(
            block.text(),
            "正文\nid:: 11111111-2222-3333-4444-555555555555\ncollapsed:: true"
        );
        assert_eq!(keys(block.properties()), vec!["id", "collapsed"]);
        assert_eq!(block.properties()[0].line(), 4);
        assert_eq!(
            block.property("collapsed").map(Property::value),
            Some("true")
        );
        // Page properties stay on the page, not on the first block.
        assert!(block.property("title").is_none());
    }

    #[test]
    fn property_values_are_raw_text() {
        // Trimming is the only change: an empty value, a value containing `::`, and a value with
        // inner spacing all survive as written.
        let page = facts("empty::\nurl:: a:: b\n  spaced::   x  y  \n");
        let values: Vec<&str> = page
            .properties()
            .iter()
            .map(|property| property.value())
            .collect();
        assert_eq!(values, vec!["", "a:: b", "x  y"]);
    }

    #[test]
    fn repeated_property_keys_keep_every_line_and_lookup_the_last() {
        let facts = facts("- 正文\n  id:: first\n  ID:: second\n");

        let block = &facts.blocks()[0];
        assert_eq!(keys(block.properties()), vec!["id", "ID"]);
        assert_eq!(block.property("id").map(Property::value), Some("second"));
    }

    #[test]
    fn property_lines_inside_code_regions_are_not_properties() {
        // A fenced block in a page preamble.
        let fenced = facts("```\nkey:: value\n```\n\n- 正文\n");
        assert!(fenced.properties().is_empty());

        // A fenced block and a `#+BEGIN_*` region inside a block's own text.
        let block = facts(
            "- 正文\n  ```\n  fake:: value\n  ```\n  #+BEGIN_SRC text\n  also-fake:: value\n  #+END_SRC\n  real:: value\n",
        );
        assert_eq!(keys(block.blocks()[0].properties()), vec!["real"]);
    }

    #[test]
    fn prose_lines_that_look_like_properties_are_rejected() {
        let prose = facts(
            "12:: 数字开头\nfoo bar:: 有空格\n[[链接:: 别名]]\na:b:: 双冒号前有冒号\n`key:: value`\nkey: value\n\n- 正文\n",
        );
        assert!(prose.properties().is_empty());

        let accepted = facts("股票代码:: 002565\n  indented:: ok\n");
        assert_eq!(keys(accepted.properties()), vec!["股票代码", "indented"]);
    }
}
