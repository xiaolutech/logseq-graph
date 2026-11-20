//! Property edits and property line helpers.

use super::super::facts::{BlockFacts, PageFacts};
use super::super::parser::{marker_content_start, PageDocument, UTF8_BOM};
use super::error::EditError;
use super::locator::{block_shapes, facts_of, BlockLocator};
use super::render::indent_of;

impl PageDocument {
    pub(crate) fn block_durable_id(
        &self,
        locator: BlockLocator<'_>,
    ) -> Result<Option<crate::domain::BlockId>, EditError> {
        let facts = PageFacts::extract(self);
        let span = Self::block_span(&facts, self, &locator)?;
        Ok(facts_of(&facts, span).durable_id())
    }

    /// Set `key:: value` on the block the locator names, and return the new source text.
    ///
    /// When the block already states the key — compared by normalized key, so `ID::` and `id::`
    /// name one property — the line stating it is replaced: the key keeps the spelling the file
    /// wrote, and only the text after `::` changes. When it does not, one new line is written
    /// after the block's last property line, or directly after the block's marker line when it
    /// states no property at all — the place between the block and its first child. Every other
    /// byte of the document is copied unchanged, so the other property lines, the marker, the
    /// children, the BOM, each line's ending, and a missing final newline all survive.
    ///
    /// The new line is rendered `{indent}{key}:: {value}` at the block's content column, with the
    /// indentation unit `Self::indent_unit` reads from the document, so a tab-indented page gains
    /// tabs. An empty `value` renders as `{key}::`, the way Logseq writes an empty property.
    ///
    /// `id::` and `collapsed::` are ordinary properties here. Upserting `id::` assigns a durable id
    /// — ADR-0009's explicit write, which is the only way one is created — and removing it with
    /// [`Self::remove_block_property_text`] breaks every `((...))` reference that pointed at the
    /// block, because the file no longer states the identity those references name.
    ///
    /// No file is read or written and no index is touched; the caller owns the page's bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::InvalidPropertyKey`] when `key` is empty, contains `::` or a line
    /// break, or has leading or trailing whitespace, and [`EditError::MultilineContent`] when
    /// `value` contains a line break, because a property line is one source line. Returns
    /// [`EditError::BlockNotFound`] when no block states the locator,
    /// [`EditError::AmbiguousBlockId`] when more than one states the id, and
    /// [`EditError::ContinuationChangesStructure`] when the rendered line would not read back as a
    /// property of the same block — the parser is the authority, so a line that would land inside a
    /// code fence or parse as a block marker is refused rather than written.
    pub fn upsert_block_property_text(
        &self,
        locator: BlockLocator<'_>,
        key: &str,
        value: &str,
    ) -> Result<String, EditError> {
        validate_property_key(key)?;
        if value.contains('\n') || value.contains('\r') {
            return Err(EditError::MultilineContent);
        }

        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let block = facts_of(&facts, own_span);

        let lines = self.lines();
        let Some(property) = block.property(key) else {
            let at = property_insertion_line(block, own_span.start() + 1);
            let marker_line = lines[own_span.start()].text();
            let align = marker_content_start(marker_line).unwrap_or(marker_line.len());
            let indent = indent_of(&self.indent_unit(), align);
            let line = replace_property_value(&format!("{indent}{key}"), value);
            let candidate = self.splice_in(at, &line);

            // Writing bytes that look edited is not the same as writing bytes that are the edit.
            // The property line must read back as a property of the same block: a key like `* x`
            // renders a block marker, and a line inserted into an unclosed fence becomes that
            // fence's content. The parser is the authority, so the result is parsed and both are
            // checked, the same way `update_block_text` checks its own rendering.
            let after = PageDocument::parse(&candidate);
            if block_shapes(&after) != block_shapes(self) {
                return Err(EditError::ContinuationChangesStructure);
            }
            let stated = PageFacts::extract(&after)
                .blocks()
                .iter()
                .find(|block| block.own_span().start() == own_span.start())
                .and_then(|block| block.property(key))
                .is_some_and(|property| property.value() == value.trim());
            if !stated {
                return Err(EditError::ContinuationChangesStructure);
            }
            return Ok(candidate);
        };

        // The key's written spelling is read back from the line itself, so `ID::` stays `ID::` and
        // whatever whitespace the file put before `::` stays there too.
        let source = lines[property.line()].text();
        #[expect(
            clippy::expect_used,
            reason = "`property` came out of `BlockFacts::properties`, which records a line only \
                      when its first `::` makes a key, so the line `property.line()` names states \
                      the separator"
        )]
        let separator = source.find("::").expect("a property line states `::`");
        let line = replace_property_value(&source[..separator], value);
        Ok(self.splice_line(property.line(), Some(&line)))
    }

    /// Remove the `key:: value` line the block the locator names, and return the new source text.
    ///
    /// The line is found through the facts by normalized key, exactly as
    /// [`Self::upsert_block_property_text`] finds the line it replaces, and only that line is cut:
    /// every other byte of the document is copied unchanged. Removing `id::` deletes the block's
    /// durable identity, which is a legal explicit write (ADR-0009) with a cost the caller owns:
    /// every `((...))` reference that pointed at the block no longer resolves, here or in any
    /// Logseq client that reads the graph.
    ///
    /// No file is read or written and no index is touched; the caller owns the page's bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::InvalidPropertyKey`] under the same rule
    /// [`Self::upsert_block_property_text`] applies, [`EditError::BlockNotFound`] when no block
    /// states the locator, [`EditError::AmbiguousBlockId`] when more than one states the id, and
    /// [`EditError::PropertyNotFound`] when the block states no property with the key.
    pub fn remove_block_property_text(
        &self,
        locator: BlockLocator<'_>,
        key: &str,
    ) -> Result<String, EditError> {
        validate_property_key(key)?;

        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let block = facts_of(&facts, own_span);
        let property = block.property(key).ok_or(EditError::PropertyNotFound)?;

        Ok(self.splice_line(property.line(), None))
    }

    /// The document with `line` inserted before source line `at`, every other line byte for byte.
    ///
    /// The inserted line takes the ending of the line that now follows it, so a CRLF file stays
    /// CRLF and a file without a final newline keeps none; the BOM survives.
    ///
    /// `at == lines.len()` appends at the end of the document, where there is no following line to
    /// take an ending from. The document's final-newline state decides instead, exactly as
    /// [`Self::insert_last_line`] decides it for an appended block: a document that ended with a
    /// newline still does, and one that ended without keeps none — its old last line is given the
    /// ending the new line needs to start on its own line. Neither state is invented or dropped,
    /// which is the same rule the `at < lines.len()` branch keeps.
    ///
    /// A document with no lines has no insertion point at all, so `at` must name a line there. No
    /// caller reaches it: every edit that inserts a line resolves a block first, and a block needs a
    /// line.
    fn splice_in(&self, at: usize, line: &str) -> String {
        let lines = self.lines();

        let mut out = String::new();
        if self.has_bom() {
            out.push(UTF8_BOM);
        }
        for source_line in &lines[..at] {
            source_line.write_to(&mut out);
        }
        // The appended line is the document's last one, so there is no next line whose ending it
        // could inherit; the file's own final-newline state is the only authority left.
        if at == lines.len() && !lines.is_empty() {
            if lines.last().is_some_and(|last| !last.has_ending()) {
                out.push_str(self.line_ending(at).as_str());
                out.push_str(line);
            } else {
                out.push_str(line);
                out.push_str(self.line_ending(at).as_str());
            }
            return out;
        }
        out.push_str(line);
        if let Some(ending) = lines[at].ending().or_else(|| Some(self.line_ending(at))) {
            out.push_str(ending.as_str());
        }
        for source_line in &lines[at..] {
            source_line.write_to(&mut out);
        }

        out
    }

    /// The document with one line replaced by `line`, or removed when `line` is `None`, and every
    /// other line byte for byte.
    ///
    /// The replaced line keeps its own ending — including none, when it was the file's last — and
    /// the BOM and a missing final newline survive, which is what makes this the one primitive both
    /// property writes render their edit through.
    fn splice_line(&self, at: usize, line: Option<&str>) -> String {
        let lines = self.lines();

        let mut out = String::new();
        if self.has_bom() {
            out.push(UTF8_BOM);
        }
        for source_line in &lines[..at] {
            source_line.write_to(&mut out);
        }
        if let Some(line) = line {
            out.push_str(line);
            if let Some(ending) = lines[at].ending() {
                out.push_str(ending.as_str());
            }
        }
        for source_line in &lines[at + 1..] {
            source_line.write_to(&mut out);
        }

        out
    }
}

/// `width` characters of indentation built from `unit`.
///
/// `unit` is never empty, so the loop terminates and the truncation only matters when `width` is not
/// a multiple of the unit's own width, as with a heading marker's content column.
/// The line a new property is inserted on: one past the last property the block states, or
/// `fallback` when it states none — the first own line after the marker, where a first property
/// belongs and before the first child.
fn property_insertion_line(block: &BlockFacts, fallback: usize) -> usize {
    block
        .properties()
        .last()
        .map_or(fallback, |property| property.line() + 1)
}

/// The property lines of `block`, split into the ones before its first content line and the ones
/// after its last one, each as an offset inside the block's own lines.
///
/// A property line in neither list sits between two content lines: the block's content is
/// interrupted by a property, and an edit of the content has no single place to write it back.
/// [`PageDocument::set_block_text_text`] refuses that layout instead of choosing a side for it.
pub(super) fn property_sides(block: &BlockFacts) -> (Vec<usize>, Vec<usize>) {
    let content = block.editable_line_offsets();
    let mut leading = Vec::new();
    let mut trailing = Vec::new();
    for property in block.properties() {
        let offset = property.line() - block.own_span().start();
        if content.first().is_none_or(|first| offset < *first) {
            leading.push(offset);
        } else if content.last().is_some_and(|last| offset > *last) {
            trailing.push(offset);
        }
    }
    (leading, trailing)
}

/// Refuse a key no editor could have written as one property line.
///
/// Empty, `::`, a line break, or leading or trailing whitespace would either stop the line from
/// parsing as a property or change its meaning between write and read, so the edit is refused
/// before any byte moves.
fn validate_property_key(key: &str) -> Result<(), EditError> {
    if key.trim().is_empty()
        || key.contains("::")
        || key.contains(['\n', '\r'])
        || key != key.trim()
    {
        return Err(EditError::InvalidPropertyKey);
    }
    Ok(())
}

/// `{key_prefix}:: {value}`, or `{key_prefix}::` for an empty value.
///
/// A property line is one source line, so the line break a value may not contain is refused by
/// the caller; here the only question is whether an empty value leaves a trailing space, which
/// Logseq does not write and a later parse would trim away.
fn replace_property_value(key_prefix: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{key_prefix}::")
    } else {
        format!("{key_prefix}:: {value}")
    }
}

/// Property lines a block states, as the key written and the line offset inside its own span.
///
/// The offset is promised, not only the key: a property line that moved to another line of the block
/// while the key stayed the same is a layout the edit never asked for, and the file is what the
/// projection reads next. The key is compared as written, because a property line is copied from the
/// source byte for byte and no edit here has a reason to respell one.
pub(super) fn property_lines(block: &BlockFacts) -> Vec<(String, usize)> {
    block
        .properties()
        .iter()
        .map(|property| {
            (
                property.key().to_owned(),
                property.line() - block.own_span().start(),
            )
        })
        .collect()
}
