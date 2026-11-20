//! Indentation and outdentation edits.

use super::super::facts::PageFacts;
use super::super::parser::PageDocument;
use super::error::EditError;
use super::locator::{block_path, find_block, subtree_span, BlockLocator, MoveTarget};
use super::render::indent_width;
use super::subtree::{pasted_start, place_of};
use super::verify::{expect_moved, Moved};

impl PageDocument {
    /// Make the block the locator names, and every block under it, the last child of its previous
    /// sibling, and return the new source text.
    ///
    /// This is Tab: the block keeps its own lines and its whole subtree, its text, its marker, and
    /// its properties, and only the leading whitespace of every moved line changes, by one
    /// indentation unit taken from the document (`Self::indent_unit`) — a tab-indented page gains
    /// tabs. Relative indentation inside the subtree is untouched, so the subtree keeps its shape.
    ///
    /// The move rides [`Self::move_block_text`], which is the same cut and paste with the same
    /// ending rules, and then checks its own claims about the bytes it produced.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::NoPreviousSibling`] when the block is the first at its level, and
    /// [`EditError::BlockNotFound`] / [`EditError::AmbiguousBlockId`] / [`EditError::PathOutOfRange`]
    /// when the locator names no block of this page. Returns
    /// [`EditError::ContinuationChangesStructure`] when the moved lines would not read back as the
    /// same subtree one level deeper.
    pub fn indent_block_text(&self, locator: BlockLocator<'_>) -> Result<String, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let place = place_of(self, own_span);
        let Some(previous) = place.previous else {
            return Err(EditError::NoPreviousSibling);
        };
        let subtree = subtree_span(self, own_span);
        let insert = subtree_span(self, previous).end();
        let indent = indent_width(self.lines()[previous.start()].text())
            + self.indent_unit().chars().count();
        let path = block_path(self, previous);
        let source =
            self.move_block_text(locator, MoveTarget::LastChildOf(BlockLocator::Path(&path)))?;
        #[expect(
            clippy::expect_used,
            reason = "`previous` is `place.previous`, which `place_of` read off this document's \
                      `children()` walk"
        )]
        let previous_block = find_block(self, previous).expect("the place names a block");
        let mut neighbours = vec![(previous, previous_block.children().len() + 1)];
        // The level the block leaves loses one child, unless it was top-level and has no block to ask.
        if let Some(parent) = place.parent {
            #[expect(
                clippy::expect_used,
                reason = "`parent` is `place.parent`, taken from the same `children()` walk"
            )]
            let parent_block = find_block(self, parent).expect("the place names a block");
            neighbours.push((parent, parent_block.children().len() - 1));
        }
        expect_moved(
            self,
            &facts,
            &source,
            Moved {
                block: own_span,
                start: pasted_start(subtree, insert),
                indent,
                parent: Some(previous.start()),
            },
            &neighbours,
        )?;
        Ok(source)
    }

    /// Make the block the locator names, and every block under it, the next sibling of its parent,
    /// and return the new source text.
    ///
    /// This is Shift-Tab. The block leaves its parent and lands immediately after the parent's whole
    /// subtree, so it keeps its place among the parent's remaining children's successors and its text,
    /// marker, properties, and children are untouched. Only the leading whitespace of every moved
    /// line changes, by one indentation unit `shift_indent` strips — a unit the document states, not
    /// one this edit guesses.
    ///
    /// The move rides [`Self::move_block_text`], which is the same cut and paste with the same
    /// ending rules, and then checks its own claims about the bytes it produced.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::TopLevelCannotOutdent`] for a top-level block, which has no parent level
    /// to move to, and [`EditError::BlockNotFound`] / [`EditError::AmbiguousBlockId`] /
    /// [`EditError::PathOutOfRange`] when the locator names no block of this page. Returns
    /// [`EditError::ContinuationChangesStructure`] when the moved lines would not read back as the
    /// same subtree one level shallower.
    pub fn outdent_block_text(&self, locator: BlockLocator<'_>) -> Result<String, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let place = place_of(self, own_span);
        let Some(parent) = place.parent else {
            return Err(EditError::TopLevelCannotOutdent);
        };
        let subtree = subtree_span(self, own_span);
        let insert = subtree_span(self, parent).end();
        let indent = indent_width(self.lines()[parent.start()].text());
        let path = block_path(self, parent);
        let source = self.move_block_text(locator, MoveTarget::After(BlockLocator::Path(&path)))?;
        #[expect(
            clippy::expect_used,
            reason = "`parent` is `place.parent` from `place_of(self, own_span)`, so it names one of \
                      this document's blocks"
        )]
        let parent_block = find_block(self, parent).expect("the place names a block");
        // The block becomes its parent's next sibling, so the level above the parent adopts it — the
        // top level when the parent was top-level.
        let grandparent = place_of(self, parent).parent;
        let mut neighbours = vec![(parent, parent_block.children().len() - 1)];
        if let Some(grandparent) = grandparent {
            #[expect(
                clippy::expect_used,
                reason = "`grandparent` is `place_of(self, parent).parent`, one more step of the \
                          same `children()` walk"
            )]
            let grandparent_block = find_block(self, grandparent).expect("the place names a block");
            neighbours.push((grandparent, grandparent_block.children().len() + 1));
        }
        expect_moved(
            self,
            &facts,
            &source,
            Moved {
                block: own_span,
                start: pasted_start(subtree, insert),
                indent,
                parent: grandparent.map(|grandparent| grandparent.start()),
            },
            &neighbours,
        )?;
        Ok(source)
    }
}
