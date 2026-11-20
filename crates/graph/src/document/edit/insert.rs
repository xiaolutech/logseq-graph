//! Insertion of new blocks.

use super::super::facts::PageFacts;
use super::super::line::LineSpan;
use super::super::parser::PageDocument;
use super::error::EditError;
use super::locator::{block_path, BlockLocator, MoveTarget};
use super::render::indent_of;
use super::subtree::PastedLine;
use super::BLOCK_MARKER;

impl PageDocument {
    /// Insert one new block whose content is `text` at the position `target` names, and return the
    /// new source text with the preorder path the new block holds in it as `(text, path)`.
    ///
    /// The new line is `target_indent` characters of whitespace + `"- " + text`, where the indent
    /// and the insertion point are exactly the ones `Self::paste_point` computes for a paste, and
    /// the indentation unit is the document's own (`Self::indent_unit`) — a tab-indented page
    /// gains tabs. The new block has no children, so its subtree is the one line.
    ///
    /// The path is parsed back out of the candidate text rather than derived from line numbers: the
    /// candidate is parsed with the same parser that reads the file, so code fences, `#+BEGIN_`
    /// regions, and mixed line endings cannot make a hand-computed position disagree with what the
    /// file will actually hold. The path follows [`BlockLocator::Path`]'s rule, which is the same
    /// rule the index reads a handle's path with: `path[0]` is the new block's preorder among the
    /// page's blocks, and every later element a sibling position, one level per element.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::EmptyContent`] for empty or whitespace-only content,
    /// [`EditError::MultilineContent`] for content containing a line break,
    /// [`EditError::MoveTargetNotFound`] when no block states the anchor's locator, and
    /// [`EditError::EndsInsideOpaqueRegion`] when the insertion point lies inside an opaque
    /// `#+BEGIN_` region, so the new line would become that region's content instead of a
    /// block. Unlike a move, an insertion cuts nothing out, so
    /// [`EditError::TargetInsideMovedSubtree`] cannot arise: the anchor's own lines stay where they
    /// are, and only the new line is added.
    ///
    /// One content shape is refused by the caller's next read rather than here: `text` that reads
    /// as a property line (`id:: <uuid>`) is written verbatim, so the new block states that
    /// property exactly as if the caller had typed it in an editor, and the projection decides what
    /// it means. A second line to carry the property would be multi-line content and is rejected
    /// above; a caller that wants durable identity writes it as Logseq does, on the block's own
    /// line.
    pub fn insert_block_text(
        &self,
        target: MoveTarget<'_>,
        text: &str,
    ) -> Result<(String, Vec<u32>), EditError> {
        if text.trim().is_empty() {
            return Err(EditError::EmptyContent);
        }
        if text.contains('\n') || text.contains('\r') {
            return Err(EditError::MultilineContent);
        }

        let facts = PageFacts::extract(self);
        let anchor_span =
            Self::block_span(&facts, self, target.anchor()).map_err(|error| match error {
                EditError::BlockNotFound => EditError::MoveTargetNotFound,
                other => other,
            })?;

        let unit = self.indent_unit();
        let (insert, target_indent) = Self::paste_point(self, anchor_span, target);
        let pasted = [PastedLine {
            text: format!("{}{BLOCK_MARKER}{text}", indent_of(&unit, target_indent)),
            // A single-line run is always the run's own junction: `paste_subtree` takes the ending
            // of the line that follows the insertion point, or the document's final-newline state
            // when the run lands at the end, exactly as the last line of any paste is decided.
            ending: None,
        }];
        let candidate = self.paste_subtree(insert, LineSpan::new(0, 0), &pasted);

        // The path is read from the same parse the caller's next read will see. The inserted line is
        // line `insert` of the candidate, because nothing before it moved, and a line that starts a
        // block is the marker line of exactly one block. A line that starts none became the content
        // of an unclosed code fence or `#+BEGIN_` region the anchor sits inside — the insertion
        // point is then inside that region, not between blocks — so it is refused, the same rule
        // [`Self::append_block`] applies to the page's tail.
        let parsed = PageDocument::parse(&candidate);
        let inserted = parsed
            .blocks_in_preorder()
            .into_iter()
            .find(|block| block.own_span().start() == insert)
            .ok_or(EditError::EndsInsideOpaqueRegion)?;
        Ok((candidate, block_path(&parsed, inserted.own_span())))
    }

    /// Insert several new blocks whose contents are `texts`, in order, at the position `target`
    /// names, and return the new source text with each new block's preorder path.
    ///
    /// The first block lands exactly where [`Self::insert_block_text`] would put one; each later
    /// block is inserted immediately after the previous one, so the blocks come out as siblings at
    /// the indentation level `target` states, in the order `texts` gives. Every position is
    /// resolved against the document the previous insertion produced — the run is re-parsed after
    /// each block, because the previous block's path in the edited text is what the next insertion
    /// anchors on — which is the same parse-back-the-candidate rule every edit here applies.
    ///
    /// The batch is all or nothing: every text is validated before the first line is inserted, so
    /// one illegal text refuses the whole run and the answer is this document's own bytes, and a
    /// refusal at any later block — an anchor the document cannot resolve, an insertion point
    /// inside an unclosed opaque region — refuses the run after bytes have been assembled but
    /// before anything is returned; no caller sees a partial run, because the caller writes only
    /// the returned text.
    ///
    /// No file is read or written and no index is touched; the caller owns the page's bytes.
    ///
    /// # Errors
    ///
    /// The refusals are [`Self::insert_block_text`]'s: [`EditError::EmptyContent`] and
    /// [`EditError::MultilineContent`] for any one of `texts`, [`EditError::MoveTargetNotFound`]
    /// when no block states the anchor's locator, and [`EditError::EndsInsideOpaqueRegion`] when an
    /// insertion point is inside an unclosed code fence or `#+BEGIN_` region. An empty `texts` is
    /// not an error: nothing is inserted and the document's own bytes come back unchanged.
    pub fn insert_blocks_text(
        &self,
        target: MoveTarget<'_>,
        texts: &[&str],
    ) -> Result<(String, Vec<Vec<u32>>), EditError> {
        for text in texts {
            if text.trim().is_empty() {
                return Err(EditError::EmptyContent);
            }
            if text.contains('\n') || text.contains('\r') {
                return Err(EditError::MultilineContent);
            }
        }

        let mut paths = Vec::with_capacity(texts.len());
        let mut candidate = self.to_source();
        let mut next_target = target;
        for text in texts {
            // The document the next insertion anchors on is the one the last insertion produced,
            // re-parsed from the candidate bytes — the first round parses this document's own
            // bytes, the same parse `insert_block_text` would run.
            let document = PageDocument::parse(&candidate);
            let (edited, path) = document.insert_block_text(next_target, text)?;
            candidate = edited;
            paths.push(path);
            // The next block follows the one just inserted, as its next sibling, which is where
            // the path this round answers says it landed in the text just assembled.
            next_target = MoveTarget::After(BlockLocator::Path(&paths[paths.len() - 1]));
        }
        Ok((candidate, paths))
    }
}
