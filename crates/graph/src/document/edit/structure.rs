//! Splitting, merging, and unwrapping blocks.

use super::super::facts::PageFacts;
use super::super::line::SourceLine;
use super::super::parser::PageDocument;
use super::error::EditError;
use super::locator::{facts_of, find_block, subtree_span, BlockLocator};
use super::property::property_lines;
use super::render::{indent_width, shift_indent, Piece};
use super::subtree::{place_of, PastedLine};
use super::verify::{rendered_properties, reparse_expect, Expected, ExpectedBlock, TextMatch};

impl PageDocument {
    /// Split the block the locator names at `offset` of its editable text, and return the new source
    /// text.
    ///
    /// `offset` counts UTF-8 bytes of [`crate::document::BlockFacts::editable_text`], which is the block's own lines
    /// with its property lines removed: a caret sits in the text an editor shows, and a property is
    /// written through its own command. `0` and the text's length are both legal, so either half may
    /// be empty.
    ///
    /// The block keeps its indentation, its marker bytes, every property line, and every child; its
    /// editable text becomes the first half. The second half becomes a new block immediately after
    /// the original's whole subtree, which is what an outliner does when Enter lands in a block that
    /// has children: the children stay where they are and belong to the first half. The new block
    /// carries the same indentation and the same marker character and nothing else — no children, no
    /// `id::`, no `collapsed::`, no other property, because a split moves text and never identity.
    ///
    /// Continuation lines are indented to each block's own content column with the document's own
    /// indentation unit, so a tab-indented page gains tabs. Every byte this edit does not render is
    /// copied from the source: the BOM, each line's own ending, the preamble, the trailing blank
    /// lines, and a missing final newline.
    ///
    /// The leading whitespace run of the second half cannot survive. A block's text can never begin
    /// with whitespace, because the parser strips the whole run of spaces and tabs after the marker,
    /// so a tail that begins with one keeps those bytes after its own marker and reads back without
    /// them — two spaces become none, not one. That run is where the offset itself falls when a caller
    /// splits between two words, and refusing it would refuse the ordinary case.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::TextOffsetOutOfRange`] when `offset` is past the end of the editable
    /// text, [`EditError::TextOffsetSplitsCharacter`] when it lands inside a character,
    /// [`EditError::BlockNotFound`] / [`EditError::AmbiguousBlockId`] / [`EditError::PathOutOfRange`]
    /// when the locator names no block of this page, and
    /// [`EditError::ContinuationChangesStructure`] when the halves would not read back as two blocks
    /// carrying exactly that text — a continuation line that becomes a marker line or a property
    /// line, a marker line that stops being one, or a new block whose last line would become page
    /// trailing space.
    pub fn split_block_text(
        &self,
        locator: BlockLocator<'_>,
        offset: usize,
    ) -> Result<String, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let block = facts_of(&facts, own_span);
        let place = place_of(self, own_span);
        let subtree = subtree_span(self, own_span);

        let text = block.editable_text();
        if offset > text.len() {
            return Err(EditError::TextOffsetOutOfRange {
                offset,
                len: text.len(),
            });
        }
        if !text.is_char_boundary(offset) {
            return Err(EditError::TextOffsetSplitsCharacter { offset });
        }
        let (head, tail) = text.split_at(offset);

        let head_lines = self.render_text_lines(block, head);
        let tail_lines = self.content_lines(&self.marker_prefix(own_span), tail);
        // A blank line that ends the new block has nowhere to go when nothing but blank lines follows
        // the subtree: the parser counts those as page trailing space, so the new block would lose
        // that line. The first half can never end that way — the new block always follows it.
        if tail_lines.last().is_some_and(|line| line.trim().is_empty())
            && self.lines()[subtree.end()..]
                .iter()
                .all(SourceLine::is_blank)
        {
            return Err(EditError::ContinuationChangesStructure);
        }

        let marker_ending = self.lines()[own_span.start()]
            .ending()
            .unwrap_or_else(|| self.line_ending(own_span.start()));
        let head_pasted: Vec<PastedLine> = head_lines
            .iter()
            .map(|line| PastedLine {
                text: line.text.clone(),
                ending: Some(marker_ending),
            })
            .collect();

        // The new block is the original's next sibling, so it lands after the original's whole
        // subtree. Only the original's own lines changed length, and they shrank, so the insertion
        // point moves up by the lines the head no longer needs.
        let insert = subtree.end() + head_lines.len() - own_span.len();
        let junction = self.line_ending(insert);
        let tail_pasted: Vec<PastedLine> = tail_lines
            .iter()
            .enumerate()
            .map(|(position, line)| PastedLine {
                text: line.clone(),
                // A run that lands at the document's end leaves its last line's ending to the
                // renderer, which keeps the file's final-newline state; every other line takes the
                // ending of the line the run is pasted before.
                ending: (position + 1 < tail_lines.len() || insert < self.lines().len())
                    .then_some(junction),
            })
            .collect();

        let mut order: Vec<Piece<'_>> = Vec::new();
        for (index, line) in self.lines()[..own_span.start()].iter().enumerate() {
            order.push(Piece::Line(index, line));
        }
        order.extend(head_pasted.iter().map(Piece::Pasted));
        for (offset, line) in self.lines()[own_span.end()..subtree.end()]
            .iter()
            .enumerate()
        {
            order.push(Piece::Line(own_span.end() + offset, line));
        }
        order.extend(tail_pasted.iter().map(Piece::Pasted));
        for (offset, line) in self.lines()[subtree.end()..].iter().enumerate() {
            order.push(Piece::Line(subtree.end() + offset, line));
        }
        let source = self.render_order(&order);

        #[expect(
            clippy::expect_used,
            reason = "`own_span` is what `block_span` answered earlier in this function, so a walk \
                      of this same document finds the block those lines belong to"
        )]
        let placed = find_block(self, own_span).expect("the locator resolved through this tree");
        let indent = indent_width(self.lines()[own_span.start()].text());
        // Neither half changes level: the head keeps the block's place, and the new block is its next
        // sibling.
        let parent = place.parent.map(|parent| parent.start());
        let expected = Expected {
            blocks: self.blocks_in_preorder().len() + 1,
            blocks_at: vec![
                ExpectedBlock {
                    start: own_span.start(),
                    parent,
                    marker: placed.marker(),
                    indent,
                    text: head.to_owned(),
                    text_match: TextMatch::Exact,
                    properties: rendered_properties(block, &head_lines),
                    children: placed.children().len(),
                },
                ExpectedBlock {
                    start: insert,
                    parent,
                    marker: placed.marker(),
                    indent,
                    text: tail.to_owned(),
                    text_match: TextMatch::LeadingWhitespaceStripped,
                    properties: Vec::new(),
                    children: 0,
                },
            ],
        };
        reparse_expect(&source, &expected)?;
        Ok(source)
    }

    /// Merge the block the locator names into the block above it, and return the new source text.
    ///
    /// The block above is the previous sibling when the block has one, and otherwise the parent the
    /// block is the first child of: nothing but that parent's own lines sits before it. In both cases
    /// the target's editable text grows by the merged block's editable text, joined with no
    /// separator — an editor merges two blocks into one run of text and does not invent spelling — and
    /// the merged block's children are adopted in order, in the place the merged block itself held:
    /// after the target's own children when the target is a previous sibling, and ahead of the
    /// parent's remaining children when the target is the parent. The adopted children keep their
    /// relative indentation and move to the column the target's children use. The merged block is
    /// deleted: its own lines, its property lines included.
    ///
    /// The target keeps its indentation, its marker bytes, and every property line, so a merge never
    /// copies an `id::` from the block it deletes and never reopens a folded block. `collapsed::`
    /// belongs to the target and stays its own too.
    ///
    /// Every byte outside the edited lines is copied from the source: the BOM, each line's own
    /// ending, the preamble, the trailing blank lines, and a missing final newline.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::NoPreviousSibling`] for a top-level first block, which has nothing above
    /// it, [`EditError::BlockNotFound`] / [`EditError::AmbiguousBlockId`] /
    /// [`EditError::PathOutOfRange`] when the locator names no block of this page, and
    /// [`EditError::ContinuationChangesStructure`] when the joined text would not read back as that
    /// one block — a line of the merged block that becomes a property line, a fence that swallows a
    /// following block, or a target that stops being a block at all.
    pub fn merge_block_text(&self, locator: BlockLocator<'_>) -> Result<String, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let block = facts_of(&facts, own_span);
        let place = place_of(self, own_span);
        // The target is the previous sibling, or else the parent this block is the first child of.
        // Which one it is decides the child count below: the parent loses this block, a sibling does
        // not.
        let (target, into_parent) = match (place.previous, place.parent) {
            (Some(previous), _) => (previous, false),
            (None, Some(parent)) => (parent, true),
            (None, None) => return Err(EditError::NoPreviousSibling),
        };
        let target_facts = facts_of(&facts, target);
        let merged = format!("{}{}", target_facts.editable_text(), block.editable_text());
        let rendered = self.render_text_lines(target_facts, &merged);

        let subtree = subtree_span(self, own_span);
        let unit = self.indent_unit();
        let delta = indent_width(self.lines()[target.start()].text()) as isize
            - indent_width(self.lines()[own_span.start()].text()) as isize;

        let marker_ending = self.lines()[target.start()]
            .ending()
            .unwrap_or_else(|| self.line_ending(target.start()));
        let rendered_pasted: Vec<PastedLine> = rendered
            .iter()
            .enumerate()
            .map(|(offset, line)| PastedLine {
                text: line.text.clone(),
                // A rendered line keeps the ending of the source line it replaces; one the target
                // did not have takes the marker line's ending, so a CRLF page stays CRLF.
                ending: Some(if target.start() + offset < target.end() {
                    self.lines()[target.start() + offset]
                        .ending()
                        .unwrap_or(marker_ending)
                } else {
                    marker_ending
                }),
            })
            .collect();
        let adopted: Vec<PastedLine> = (own_span.end()..subtree.end())
            .map(|index| PastedLine {
                text: shift_indent(self.lines()[index].text(), delta, &unit),
                ending: self.lines()[index].ending(),
            })
            .collect();

        let mut order: Vec<Piece<'_>> = Vec::new();
        for (index, line) in self.lines()[..target.start()].iter().enumerate() {
            order.push(Piece::Line(index, line));
        }
        order.extend(rendered_pasted.iter().map(Piece::Pasted));
        for index in target.end()..own_span.start() {
            order.push(Piece::Line(index, &self.lines()[index]));
        }
        order.extend(adopted.iter().map(Piece::Pasted));
        for (offset, line) in self.lines()[subtree.end()..].iter().enumerate() {
            order.push(Piece::Line(subtree.end() + offset, line));
        }
        let source = self.render_order(&order);

        #[expect(
            clippy::expect_used,
            reason = "`own_span` is what `block_span` answered earlier in this function, so a walk \
                      of this same document finds the block those lines belong to"
        )]
        let placed = find_block(self, own_span).expect("the locator resolved through this tree");
        #[expect(
            clippy::expect_used,
            reason = "`target` is `place.previous` or `place.parent`, and `place_of` took both from \
                      this document's own `children()` walk, so it names one of its blocks"
        )]
        let target_block = find_block(self, target).expect("the place names a block of this page");
        let target_parent = place_of(self, target).parent.map(|parent| parent.start());
        let adopted_base = target.start() + rendered.len() + (own_span.start() - target.end());
        let mut blocks_at = vec![ExpectedBlock {
            start: target.start(),
            parent: target_parent,
            marker: target_block.marker(),
            indent: indent_width(self.lines()[target.start()].text()),
            text: merged,
            text_match: TextMatch::Exact,
            properties: rendered_properties(target_facts, &rendered),
            children: target_block.children().len() - usize::from(into_parent)
                + placed.children().len(),
        }];
        for child in placed.children() {
            let child_facts = facts_of(&facts, child.own_span());
            let shifted = shift_indent(self.lines()[child.own_span().start()].text(), delta, &unit);
            blocks_at.push(ExpectedBlock {
                start: adopted_base + (child.own_span().start() - own_span.end()),
                parent: Some(target.start()),
                marker: child.marker(),
                indent: indent_width(&shifted),
                text: child_facts.editable_text(),
                text_match: TextMatch::Exact,
                properties: property_lines(child_facts),
                children: child.children().len(),
            });
        }
        let expected = Expected {
            blocks: self.blocks_in_preorder().len() - 1,
            blocks_at,
        };
        reparse_expect(&source, &expected)?;
        Ok(source)
    }

    /// Delete the block the locator names and lift its children into its place, and return the new
    /// source text.
    ///
    /// This is Backspace on an empty block. The block's own lines go, property lines included, and
    /// its children take exactly the lines the block held, in the same order: the block's own content
    /// is not carried anywhere, which is what separates this from [`Self::merge_block_text`]. The
    /// lifted blocks take the lines the deleted block held: their root lands on the column the deleted
    /// block was indented to — column zero for a top-level block, its own indentation otherwise, which
    /// is the column its siblings share — and each lifted subtree keeps its inner indentation. A block
    /// whose children are indented by different widths therefore lifts each child to that one column
    /// instead of shifting them all by one guessed step.
    ///
    /// A block with no children is simply deleted, and that is the same edit: nothing is lifted.
    ///
    /// Every byte outside the edited lines is copied from the source: the BOM, each line's own
    /// ending, the preamble, the trailing blank lines, and a missing final newline.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::BlockNotFound`] / [`EditError::AmbiguousBlockId`] /
    /// [`EditError::PathOutOfRange`] when the locator names no block of this page, and
    /// [`EditError::ContinuationChangesStructure`] when the lifted lines would not read back as the
    /// same subtrees at the level above.
    pub fn unwrap_block_text(&self, locator: BlockLocator<'_>) -> Result<String, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let place = place_of(self, own_span);
        #[expect(
            clippy::expect_used,
            reason = "`own_span` is what `block_span` answered here, so a walk of this same document \
                      finds the block those lines belong to"
        )]
        let block = find_block(self, own_span).expect("the locator resolved through this tree");
        let subtree = block.subtree_span();
        let unit = self.indent_unit();
        // Every lifted block lands on the column the deleted block sat at, which is the level its
        // siblings are on: column zero for a top-level block, its own indentation otherwise. Each
        // lifted child gets its own delta, because two children of one block can be indented by
        // different widths — the parser only requires a child to be deeper than its parent, not
        // deeper by one fixed step — and one shared delta would leave the wider child indented under
        // the block above instead of at the level it was lifted to.
        let target = indent_width(self.lines()[own_span.start()].text());
        let mut lifted: Vec<PastedLine> = Vec::new();
        for child in block.children() {
            let delta = target as isize
                - indent_width(self.lines()[child.own_span().start()].text()) as isize;
            for index in child.subtree_span().start()..child.subtree_span().end() {
                lifted.push(PastedLine {
                    text: shift_indent(self.lines()[index].text(), delta, &unit),
                    ending: self.lines()[index].ending(),
                });
            }
        }

        let mut order: Vec<Piece<'_>> = Vec::new();
        for (index, line) in self.lines()[..own_span.start()].iter().enumerate() {
            order.push(Piece::Line(index, line));
        }
        order.extend(lifted.iter().map(Piece::Pasted));
        for (offset, line) in self.lines()[subtree.end()..].iter().enumerate() {
            order.push(Piece::Line(subtree.end() + offset, line));
        }
        let source = self.render_order(&order);

        // The lifted blocks take the place the deleted one held, so they answer the parent it had.
        let parent = place.parent.map(|parent| parent.start());
        let mut blocks_at = Vec::new();
        for child in block.children() {
            let child_facts = facts_of(&facts, child.own_span());
            blocks_at.push(ExpectedBlock {
                start: child.own_span().start() - own_span.len(),
                parent,
                marker: child.marker(),
                // The column the deleted block sat at, read from that line and not from the shift this
                // edit computed, so the claim and the bytes can disagree.
                indent: target,
                text: child_facts.editable_text(),
                text_match: TextMatch::Exact,
                properties: property_lines(child_facts),
                children: child.children().len(),
            });
        }
        let expected = Expected {
            blocks: self.blocks_in_preorder().len() - 1,
            blocks_at,
        };
        reparse_expect(&source, &expected)?;
        Ok(source)
    }
}
