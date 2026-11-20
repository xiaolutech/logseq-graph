//! Block marker replacement.

use super::super::facts::{BlockFacts, PageFacts};
use super::super::parser::{marker_content_start, PageDocument};
use super::error::EditError;
use super::locator::{facts_of, find_block, BlockLocator};
use super::render::{lead_whitespace, reindent, Piece};
use super::subtree::PastedLine;
use super::BLOCK_MARKER;
use crate::domain::BlockMarker;

impl PageDocument {
    /// Rewrite the marker bytes of the block a locator names, and return the new source text.
    ///
    /// Only the marker of the block's first line changes; its indentation, its content, its
    /// children, and each line's ending stay what the file states. The three wanted cases are
    /// bullet to heading, heading to bullet, and one heading level to another:
    ///
    /// - `Bullet` keeps a bullet a bullet. [`BlockMarker`] does not remember whether the file wrote
    ///   `-`, `*` or `+`, and rewriting a bullet from the enum would respell a byte no caller
    ///   mentioned, so this is a no-op.
    /// - `Heading { level }` writes `level` `#` characters and one space after them.
    /// - `Bullet` on a heading writes `- `, Logseq's default bullet. A heading has no bullet
    ///   character to restore, so the default is the only answer the file can give.
    ///
    /// A block with no content leaves a bare marker: `-` rather than `- `, the way
    /// [`Self::update_block_text`] renders an empty text. A heading keeps its space, because `##`
    /// with nothing after it is not a heading marker at all.
    ///
    /// The block's content column follows the marker's width, so its own continuation lines and
    /// property lines are re-indented to the new column: `- ` and `## ` are not the same width, and
    /// a continuation line that kept the old indentation would no longer line up with the content
    /// it continues. The new indentation is built from the document's own indentation unit
    /// (`Self::indent_unit`) at the new `marker_content_start`, with whatever indentation a line
    /// carries beyond the old column kept as it was — that is what a code fence's inner indentation
    /// is. Those lines are the only bytes this edit changes besides the marker itself. Children are
    /// left byte for byte: a child's indentation is its own depth, not its parent's marker width.
    ///
    /// Every other byte of the document is copied unchanged: the BOM, the preamble, the sibling
    /// blocks, the trailing blank lines, each line's own ending, and a missing final newline.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::InvalidHeadingLevel`] when a heading level is outside `1..=6`,
    /// [`EditError::BlockNotFound`] / [`EditError::AmbiguousBlockId`] /
    /// [`EditError::PathOutOfRange`] when the locator names no block of this page, and
    /// [`EditError::ContinuationChangesStructure`] when the rewritten lines would not read back as
    /// the same blocks with the same text, properties, and children.
    pub fn set_block_marker_text(
        &self,
        locator: BlockLocator<'_>,
        marker: BlockMarker,
    ) -> Result<String, EditError> {
        if let BlockMarker::Heading { level } = marker {
            if !(1..=6).contains(&level) {
                return Err(EditError::InvalidHeadingLevel { level });
            }
        }

        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        #[expect(
            clippy::expect_used,
            reason = "`own_span` is what `block_span` just answered for this locator: the id \
                      spelling read it off these `facts`, both path spellings off this document's \
                      `children()`, so the block that owns those lines is in this tree"
        )]
        let block = find_block(self, own_span).expect("the locator resolved through this tree");
        if block.marker() == marker {
            return Ok(self.to_source());
        }

        let lines = self.lines();
        let first_line = lines[own_span.start()].text();
        let old_column = marker_content_start(first_line).unwrap_or(first_line.len());
        let indent = lead_whitespace(first_line);
        let marker_bytes = match marker {
            BlockMarker::Bullet => BLOCK_MARKER.to_owned(),
            BlockMarker::Heading { level } => format!("{} ", "#".repeat(usize::from(level))),
        };
        let content = &first_line[old_column..];
        let mut first_rendered = format!("{indent}{marker_bytes}{content}");
        // Empty content leaves the separator the marker needs with nothing to separate, so it is
        // trimmed the way `update_block_text` trims an empty text — except for a heading, where `##`
        // with no space after it is not a heading marker at all and the space has to stay.
        if content.is_empty() && matches!(marker, BlockMarker::Bullet) {
            let kept = first_rendered.trim_end().len();
            first_rendered.truncate(kept);
        }
        // The content column is read back off the line that was just rendered, the way
        // `update_block_text` reads it, so a trimmed marker line gets its own narrower column
        // instead of the width the untrimmed marker would have had.
        let new_column = marker_content_start(&first_rendered).unwrap_or(first_rendered.len());
        let unit = self.indent_unit();
        let marker_ending = lines[own_span.start()]
            .ending()
            .unwrap_or_else(|| self.line_ending(own_span.start()));

        let pasted: Vec<PastedLine> = (own_span.start()..own_span.end())
            .map(|index| {
                let text = if index == own_span.start() {
                    first_rendered.clone()
                } else {
                    reindent(lines[index].text(), old_column, new_column, &unit)
                };
                PastedLine {
                    text,
                    ending: lines[index].ending().or(Some(marker_ending)),
                }
            })
            .collect();

        let mut order: Vec<Piece<'_>> = Vec::new();
        for (index, line) in lines[..own_span.start()].iter().enumerate() {
            order.push(Piece::Line(index, line));
        }
        order.extend(pasted.iter().map(Piece::Pasted));
        for (offset, line) in lines[own_span.end()..].iter().enumerate() {
            order.push(Piece::Line(own_span.end() + offset, line));
        }
        let source = self.render_order(&order);

        // The rewritten lines must read back as the same page: same blocks in the same place, the
        // same text, the same properties at the same offsets, and the same children. The marker is
        // the one thing that is allowed to differ, and only on the block this edit names.
        let after = PageDocument::parse(&source);
        let before_preorder = self.blocks_in_preorder();
        let after_preorder = after.blocks_in_preorder();
        let shapes_hold = before_preorder.len() == after_preorder.len()
            && before_preorder
                .iter()
                .zip(&after_preorder)
                .all(|(before, rewritten)| {
                    rewritten.own_span() == before.own_span()
                        && rewritten.indent() == before.indent()
                        && rewritten.children().len() == before.children().len()
                        && if before.own_span() == own_span {
                            rewritten.marker() == marker
                        } else {
                            rewritten.marker() == before.marker()
                        }
                });
        if !shapes_hold {
            return Err(EditError::ContinuationChangesStructure);
        }

        let before_facts = facts_of(&facts, own_span);
        let after_page_facts = PageFacts::extract(&after);
        let after_facts = facts_of(&after_page_facts, own_span);
        let stated = |block: &BlockFacts| -> Vec<(String, String, usize)> {
            block
                .properties()
                .iter()
                .map(|property| {
                    (
                        property.key().to_owned(),
                        property.value().to_owned(),
                        property.line() - block.own_span().start(),
                    )
                })
                .collect()
        };
        if after_facts.editable_text() != before_facts.editable_text()
            || stated(after_facts) != stated(before_facts)
        {
            return Err(EditError::ContinuationChangesStructure);
        }

        Ok(source)
    }
}
