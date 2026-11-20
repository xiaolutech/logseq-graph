//! A content-search hit.
//!
//! A hit answers "where does this text occur": it carries the page name as written, the block's
//! preorder position in that page, a bounded snippet around the match, and the selector the block
//! answers to. The preorder is a position inside the revision that answered the query, not a stable
//! block handle, so a caller relocates by re-reading the page tree at the same revision rather than
//! by storing the position.
//!
//! Like every domain type this is a value at one graph revision: no IO, no SQL, no storage handle.

use super::BlockSelector;

/// One character of the search fold: see [`search_key`] for the rule as a whole.
///
/// Public because the SQL scalar function the index registers folds one block text at a time and
/// needs the same per-character decision without allocating a copy when nothing changes.
pub fn search_key_char(character: char) -> char {
    // `to_lowercase` yields at least one character for every `char`; a multi-character mapping
    // (U+0130 `İ` becomes `i` plus a combining dot) keeps the character as written, because the
    // fold must not change the character count.
    let mut lower = character.to_lowercase();
    let folded = lower.next().unwrap_or(character);
    if lower.next().is_some() {
        return character;
    }
    // A non-ASCII character never folds into ASCII: U+212A KELVIN SIGN lowercases to `k`, and it
    // stays as written so an ASCII query gets the same answer from either statement of the
    // search read. This is the one place that rule is decided.
    if folded.is_ascii() && !character.is_ascii() {
        return character;
    }
    folded
}

/// Key that decides whether block text contains a searched substring: each character lowercased
/// when its lowercase mapping is one character, kept as written otherwise.
///
/// This is the text-matching rule, and it is deliberately not the page-name rule
/// ([`page_name_key`](super::page_name_key)): that key answers identity — whether two spellings
/// denote one page — and may change the length of what it folds (`İ` becomes two characters),
/// because a page key is only ever compared as a whole. This key is folded per character and keeps
/// the character count exactly, because a match position in the key must be the same position in
/// the stored text for the snippet window to cut the right characters.
///
/// - `Ärger ÜBER` and `ärger über` match each other, and either query finds either text.
/// - `Straße` and `STRASSE` do not match: `ß` has no uppercase, so this is not a `ss` fold, and no
///   normalization happens here either, exactly like [`page_name_key`](super::page_name_key).
/// - `İ` stays `İ` (its lowercase mapping is two characters), so `İstanbul` does not answer
///   `istanbul`.
/// - U+212A KELVIN SIGN stays as written (its lowercase is the ASCII `k`), so it does not answer
///   an ASCII `k` query; see [`search_key_char`].
pub fn search_key(text: &str) -> String {
    text.chars().map(search_key_char).collect()
}

/// One block whose projected text contains the searched substring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    page: String,
    preorder: u32,
    /// The identity the block answers to, beside the snippet so a caller can act on the hit — write
    /// through [`BlockSelector`] — without re-reading the page to find the same block again.
    selector: BlockSelector,
    snippet: String,
}

impl SearchHit {
    pub(crate) fn new(
        page: String,
        preorder: u32,
        selector: BlockSelector,
        snippet: String,
    ) -> Self {
        Self {
            page,
            preorder,
            selector,
            snippet,
        }
    }

    /// Page name as written by the file that produced the block.
    ///
    /// This is the stored spelling, so it can be passed back to a page read under the page-name
    /// key. It is never the spelling the query used.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Depth-first position of the block among the page's blocks, starting at zero.
    ///
    /// It is the same position [`BlockNode::preorder`](super::BlockNode::preorder) reports, which is
    /// what lets a caller locate the block in a page tree read at the same revision. It is a
    /// position, not an identifier: the next replacement of the page can put different text there.
    pub fn preorder(&self) -> u32 {
        self.preorder
    }

    /// The identity the block answers to, the same shape [`Graph::block`](crate::engine::Graph::block)
    /// reads and every write accepts.
    ///
    /// It sits beside the snippet so a hit is actionable on its own: the caller that found the text
    /// already holds the address, and no second read is needed to discover it.
    pub fn selector(&self) -> &BlockSelector {
        &self.selector
    }

    /// Context around the match, bounded in size.
    ///
    /// The snippet is a window of the stored block text, not a summary: block text and Markdown
    /// syntax are kept byte for byte, and `…` marks a truncated edge. The exact window size and the
    /// folding rule are stated by [`Graph::search`](crate::engine::Graph::search).
    pub fn snippet(&self) -> &str {
        &self.snippet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::page_name_key;

    #[test]
    fn the_search_key_folds_one_character_at_a_time_and_nothing_else() {
        assert_eq!(search_key("Ärger ÜBER"), "ärger über");
        assert_eq!(search_key("HELLO"), "hello");
        // Not a full case fold and not normalization, the same stance the page-name rule takes.
        assert_eq!(search_key("Straße"), "straße");
        assert_ne!(search_key("STRASSE"), search_key("Straße"));
        assert_ne!(search_key("\u{e9}"), search_key("e\u{301}"));
        // A mapping that would change the character count keeps the character as written.
        assert_eq!(search_key("İstanbul"), "İstanbul");
        // A non-ASCII character never folds into ASCII.
        assert_eq!(search_key("\u{212a}"), "\u{212a}");
        // The character count is the invariant: every character folds to one character.
        for text in ["Ärger ÜBER", "İstanbul \u{212a} Straße"] {
            assert_eq!(search_key(text).chars().count(), text.chars().count());
        }
    }

    #[test]
    fn the_search_key_is_not_the_page_name_key() {
        // Identity may fold to a different length and may fold into ASCII; text matching may not,
        // because a match position in the key has to name a position in the stored text.
        assert_eq!(page_name_key("İstanbul"), "i̇stanbul");
        assert_eq!(page_name_key("\u{212a}"), "k");
        assert_eq!(search_key("İstanbul"), "İstanbul");
        assert_eq!(search_key("\u{212a}"), "\u{212a}");
    }
}
