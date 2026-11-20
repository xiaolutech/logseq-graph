//! Subtree removal, movement, and pasting.

use super::super::facts::PageFacts;
use super::super::line::{LineEnding, LineSpan};
use super::super::parser::{PageDocument, UTF8_BOM};
use super::error::EditError;
use super::locator::{subtree_span, subtree_span_contains, BlockLocator, MoveTarget};
use super::render::{indent_width, shift_indent, Piece};

/// One line to paste into a document, and the ending it keeps when the paste lands it inside the
/// document rather than at its end.
pub(super) struct PastedLine {
    pub(super) text: String,
    pub(super) ending: Option<LineEnding>,
}

impl PageDocument {
    /// Delete an inclusive run of siblings and their subtrees, copying every other source byte.
    ///
    /// Both locators are resolved in this document before checking their parents and order.
    pub fn remove_block_range_text(
        &self,
        first: BlockLocator<'_>,
        last: BlockLocator<'_>,
    ) -> Result<String, EditError> {
        let first_path = self.block_child_path(first.clone())?;
        let last_path = self.block_child_path(last.clone())?;
        if first_path[..first_path.len() - 1] != last_path[..last_path.len() - 1] {
            return Err(EditError::RangeNotSiblings);
        }
        if first_path.last() > last_path.last() {
            return Err(EditError::RangeReversed);
        }
        let facts = PageFacts::extract(self);
        let first_span = Self::block_span(&facts, self, &first)?;
        let last_span = Self::block_span(&facts, self, &last)?;
        Ok(self.cut_subtree(LineSpan::new(
            first_span.start(),
            subtree_span(self, last_span).end(),
        )))
    }

    /// Delete the block that states `id:: <block_id>`, and every block under it.
    ///
    /// The block is found through the `id::` value it states, exactly as
    /// [`PageDocument::update_block_text`] finds it: matching the id ADR-0005 makes durable does not
    /// depend on a traversal order, ASCII case-insensitive comparison included. Two blocks claiming
    /// one id is corrupt input, so it is an error rather than a guess about which subtree to delete.
    ///
    /// The deleted range is the whole subtree, not one block: a child whose parent is gone is a
    /// block the outliner never had. `subtree_span` answers which lines those are, and
    /// `Self::cut_subtree` takes them out.
    ///
    /// Every byte outside that range is copied from the source unchanged, so the preamble, the
    /// sibling blocks, the trailing blank lines, each line's own ending, and the BOM all survive. A
    /// page whose only block is removed has no block left and keeps whatever preamble it had, which
    /// may be the empty string: deleting the last block is a legal state, not a refused one.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::BlockNotFound`] when no block states the id, and
    /// [`EditError::AmbiguousBlockId`] when more than one does.
    pub fn remove_block_text(&self, locator: BlockLocator<'_>) -> Result<String, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let doomed = subtree_span(self, own_span);

        Ok(self.cut_subtree(doomed))
    }

    /// The document with the lines of `doomed` removed, and every other line byte for byte.
    ///
    /// The surviving lines are written from their own bytes, so the line that now ends the file
    /// keeps the ending it had: no final newline is invented and none is removed from a line
    /// outside the deleted range.
    fn cut_subtree(&self, doomed: LineSpan) -> String {
        let lines = self.lines();

        let mut out = String::new();
        if self.has_bom() {
            out.push(UTF8_BOM);
        }
        for line in &lines[..doomed.start()] {
            line.write_to(&mut out);
        }
        for line in &lines[doomed.end()..] {
            line.write_to(&mut out);
        }

        out
    }

    /// Move the block `block` names, and every block under it, next to the block `target` names,
    /// and return the new source text.
    ///
    /// The edited range is the moved subtree, one contiguous run of source lines, and it is cut out
    /// and pasted back in one pass. Every line between the two is copied byte for byte, so the BOM,
    /// the preamble, the line endings, the trailing blank lines, and a missing final newline all
    /// survive; only the moved lines change, and only in their leading whitespace.
    ///
    /// Indentation is what makes the paste a move rather than a shuffle. The subtree keeps its
    /// internal shape: every moved line moves by the same delta, read off the source block's own
    /// marker line and the target position. A deeper position is filled with the document's own
    /// indentation unit (`Self::indent_unit`), so a tab-indented page gains tabs; a shallower one
    /// removes exactly as many leading characters as the delta and leaves the rest of the line
    /// alone. A blank line stays blank, because indenting an empty line is whitespace the file never
    /// had.
    ///
    /// Line endings belong to the position, not to the text: the moved lines keep their own endings
    /// except the last one, which takes the ending of the line that follows the insertion point.
    /// That is the byte that separates the pasted run from what comes after it, and keeping the
    /// moved line's own ending there would duplicate or drop a newline. A run pasted at the end of
    /// the file keeps the document's final-newline state.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::BlockNotFound`] when no block states the locator `block` carries
    /// ([`EditError::PathOutOfRange`] when it is a path that names none),
    /// [`EditError::AmbiguousBlockId`] when more than one block states the moved id,
    /// [`EditError::MoveTargetNotFound`] when no block states the anchor's locator, and
    /// [`EditError::TargetInsideMovedSubtree`] when the target is the moved block itself or a block
    /// inside the subtree being moved.
    pub fn move_block_text(
        &self,
        block: BlockLocator<'_>,
        target: MoveTarget<'_>,
    ) -> Result<String, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &block)?;
        // The target is found the same way, but a miss is its own error: this edit was asked with
        // two addresses, and `BlockNotFound` would not say which one the file does not state.
        let anchor_span =
            Self::block_span(&facts, self, target.anchor()).map_err(|error| match error {
                EditError::BlockNotFound => EditError::MoveTargetNotFound,
                other => other,
            })?;

        let moved = subtree_span(self, own_span);
        // A target inside the range being cut out would move the subtree into itself: the anchor
        // would no longer be where the edit looked, and a child target would make the block its own
        // descendant. Naming the moved block itself is the same case, because its own line is the
        // first line of that range.
        if moved.contains(anchor_span.start()) {
            return Err(EditError::TargetInsideMovedSubtree);
        }

        let unit = self.indent_unit();
        let lines = self.lines();
        let source_indent = indent_width(lines[moved.start()].text());
        let (insert, target_indent) = Self::paste_point(self, anchor_span, target);

        let delta = target_indent as isize - source_indent as isize;
        let pasted: Vec<PastedLine> = (moved.start()..moved.end())
            .map(|index| PastedLine {
                text: shift_indent(lines[index].text(), delta, &unit),
                ending: lines[index].ending(),
            })
            .collect();

        Ok(self.paste_subtree(insert, moved, &pasted))
    }

    /// Move the block `block` names in `source`, and every block under it, next to the block
    /// `target` names in `destination`, and return the new source text of both pages as
    /// `(source, destination)`.
    ///
    /// This is [`PageDocument::move_block_text`] with the two halves of the move read from two
    /// documents. The subtree is resolved in `source` and the anchor in `destination`
    /// independently, each through the locator it carries, so a missing address is reported per
    /// side: [`EditError::BlockNotFound`] names the moved address, [`EditError::MoveTargetNotFound`]
    /// the target address, and [`EditError::AmbiguousBlockId`] whichever document states one id
    /// twice. A target inside the moved subtree cannot arise across two documents, so that error has
    /// no meaning here.
    ///
    /// The source page loses exactly the subtree's lines, and every other byte of it is copied
    /// unchanged. The destination page gains the same lines, re-indented by one delta: the internal
    /// relative indentation of the subtree survives, its root is aligned to the anchor, and a
    /// deeper position is filled with the destination's own indentation unit — a tab-indented
    /// destination gains tabs even when the source used spaces. Line endings stay with the lines
    /// they belong to, except the last moved one, whose ending is the junction byte and follows the
    /// destination's line at the insertion point, or the destination's final-newline state when the
    /// run lands at its end. Every untouched byte of both documents survives: the BOM, the
    /// preamble, mixed line endings, trailing blank lines, and a missing final newline.
    ///
    /// No file is read or written and no index is touched; the caller owns both pages' bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::BlockNotFound`] when no block in `source` states the locator `block`
    /// carries, [`EditError::MoveTargetNotFound`] when no block in `destination` states the target
    /// locator, and [`EditError::AmbiguousBlockId`] when more than one block in either document
    /// states the id it was asked about.
    pub fn move_block_text_to_page(
        &self,
        block: BlockLocator<'_>,
        destination: &PageDocument,
        target: MoveTarget<'_>,
    ) -> Result<(String, String), EditError> {
        let source_facts = PageFacts::extract(self);
        let own_span = Self::block_span(&source_facts, self, &block)?;
        let moved = subtree_span(self, own_span);

        let destination_facts = PageFacts::extract(destination);
        // The anchor is found the same way, but a miss is its own error: this edit was asked with
        // two addresses in two documents, and `BlockNotFound` would not say which side failed.
        let anchor_span = Self::block_span(&destination_facts, destination, target.anchor())
            .map_err(|error| match error {
                EditError::BlockNotFound => EditError::MoveTargetNotFound,
                other => other,
            })?;

        let unit = destination.indent_unit();
        let source_indent = indent_width(self.lines()[moved.start()].text());
        let (insert, target_indent) = Self::paste_point(destination, anchor_span, target);

        let delta = target_indent as isize - source_indent as isize;
        let pasted: Vec<PastedLine> = (moved.start()..moved.end())
            .map(|index| PastedLine {
                text: shift_indent(self.lines()[index].text(), delta, &unit),
                ending: self.lines()[index].ending(),
            })
            .collect();

        let destination_text = destination.paste_subtree(insert, LineSpan::new(0, 0), &pasted);
        let source_text = self.cut_subtree(moved);
        Ok((source_text, destination_text))
    }

    /// Where a subtree is pasted for `target`, and how deep its root sits there.
    ///
    /// `Before` and `After` land on the anchor's own line and its subtree's end, which are the two
    /// places where a sibling can start; the child targets add one indentation unit to the
    /// anchor's own depth. Both answers are read from the document that receives the paste, which
    /// is what makes the unit — and therefore the pasted indentation — the destination's, not the
    /// source's.
    pub(super) fn paste_point(
        document: &PageDocument,
        anchor_span: LineSpan,
        target: MoveTarget<'_>,
    ) -> (usize, usize) {
        let unit = document.indent_unit();
        let anchor_indent = indent_width(document.lines()[anchor_span.start()].text());
        let anchor_subtree = subtree_span(document, anchor_span);
        match target {
            MoveTarget::Before(_) => (anchor_span.start(), anchor_indent),
            MoveTarget::After(_) => (anchor_subtree.end(), anchor_indent),
            MoveTarget::FirstChildOf(_) => {
                (anchor_span.end(), anchor_indent + unit.chars().count())
            }
            MoveTarget::LastChildOf(_) => {
                (anchor_subtree.end(), anchor_indent + unit.chars().count())
            }
        }
    }

    /// The document with `pasted` inserted before line `insert` and the lines of `skip` removed,
    /// every other line byte for byte.
    ///
    /// The one-page move passes the cut-out range as `skip` and pastes it back elsewhere in the
    /// same pass; a move between pages passes an empty span, because the cut happened in another
    /// document. `insert` must not name a line inside `skip`, which the one-page caller already
    /// guarantees by refusing a target inside the moved subtree.
    ///
    /// Endings are position rules, not text rules, and they are the same ones the one-page move
    /// has always applied. A pasted line keeps the ending it had in the document it was cut from;
    /// the last one is the junction instead, because its ending is the byte separating the pasted
    /// run from what follows: the ending of the line at the insertion point, or — when the run
    /// lands at the end of the document — the document's final-newline state. A line of this
    /// document that stops being last keeps its own ending and gains one only when it used to end
    /// the file without a newline; the line that is still last keeps its own ending, which is what
    /// preserves a missing final newline.
    pub(super) fn paste_subtree(
        &self,
        insert: usize,
        skip: LineSpan,
        pasted: &[PastedLine],
    ) -> String {
        let lines = self.lines();

        // The pieces in output order, each naming the source line it comes from: the document's own
        // lines, minus `skip`, with the pasted run spliced in before line `insert`. One pass keeps
        // every line number valid: deleting the subtree first and then searching for the insertion
        // point would run that search against a document whose indices had already moved. `insert
        // <= lines.len()`, so the run always lands.
        let mut order: Vec<Piece<'_>> = Vec::with_capacity(lines.len() + pasted.len());
        for (index, line) in lines.iter().enumerate() {
            if index == insert {
                order.extend(pasted.iter().map(Piece::Pasted));
            }
            if !skip.contains(index) {
                order.push(Piece::Line(index, line));
            }
        }
        if insert == lines.len() {
            order.extend(pasted.iter().map(Piece::Pasted));
        }

        let mut out = String::new();
        if self.has_bom() {
            out.push(UTF8_BOM);
        }
        // The moved subtree holds at least one line, so the order is never empty.
        let last = order.len() - 1;
        for (position, piece) in order.iter().enumerate() {
            match *piece {
                Piece::Line(index, line) => {
                    out.push_str(line.text());
                    // A line that is no longer last needs a newline even if the file used to end
                    // there. The line that is still last keeps its own ending, which is what
                    // preserves a missing final newline.
                    let ending = if position == last {
                        line.ending()
                    } else {
                        line.ending().or_else(|| Some(self.line_ending(index)))
                    };
                    if let Some(ending) = ending {
                        out.push_str(ending.as_str());
                    }
                }
                Piece::Pasted(line) => {
                    out.push_str(&line.text);

                    let ending = if position == last {
                        // The pasted run now ends the file. It keeps the file's final-newline
                        // state, and when there is one, its bytes follow the lines before it.
                        if self.ends_with_newline() {
                            Some(self.line_ending(insert))
                        } else {
                            None
                        }
                    } else if let Piece::Line(_, next) = order[position + 1] {
                        // The last pasted line is the junction: its ending belongs to the
                        // insertion point, so it is the ending of the line that now follows the
                        // run — the next piece in output order, which is never inside `skip`.
                        next.ending().or_else(|| Some(self.line_ending(insert)))
                    } else {
                        // Inside the run, a line keeps the ending it had in the document it was
                        // cut from. Only the last line of a source file can lack one, and the
                        // last line of the run never gets here, so this is always `Some`.
                        line.ending
                    };
                    if let Some(ending) = ending {
                        out.push_str(ending.as_str());
                    }
                }
            }
        }

        out
    }
}

/// Where a block sits among its siblings.
#[derive(Debug, Clone, Copy)]
pub(super) struct Place {
    /// The block immediately before this one at the same level, when there is one.
    pub(super) previous: Option<LineSpan>,
    /// The parent block's own lines, or `None` when this block is top-level.
    pub(super) parent: Option<LineSpan>,
}

/// Find the block whose own lines are `own` and report its neighbours at its own level.
///
/// Siblings are the only blocks the structure edits ask about — the one above it to merge into or
/// indent under, the parent to outdent to — and both are answers of the parsed tree rather than of
/// line arithmetic, because two files can spell one shape with tabs, with spaces, or with markers of
/// different widths.
pub(super) fn place_of(document: &PageDocument, own: LineSpan) -> Place {
    let mut siblings = document.blocks();
    let mut parent = None;
    loop {
        if let Some(index) = siblings.iter().position(|block| block.own_span() == own) {
            return Place {
                previous: index
                    .checked_sub(1)
                    .map(|before| siblings[before].own_span()),
                parent,
            };
        }
        #[expect(
            clippy::expect_used,
            reason = "the loop descends only into the child whose subtree holds `own`, and the roots \
                      cover every block of the page, so exactly one sibling holds it at each level"
        )]
        let next = siblings
            .iter()
            .find(|block| subtree_span_contains(block, own))
            .expect("a block of this document sits under exactly one top-level block");
        parent = Some(next.own_span());
        siblings = next.children();
    }
}

/// The output line a run pasted before `insert` starts at, after `skip` was cut out.
///
/// Every line of `skip` before the insertion point stops existing, so the run moves up by that many
/// lines: a run pasted at its own start stays put, and one pasted past its whole subtree moves up by
/// the subtree's length.
pub(super) fn pasted_start(skip: LineSpan, insert: usize) -> usize {
    let dropped = insert.saturating_sub(skip.start()).min(skip.len());
    insert - dropped
}
