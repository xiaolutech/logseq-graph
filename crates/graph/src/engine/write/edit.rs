use super::error::WriteError;
use super::target::{check_ephemeral_freshness, AnswerAddress, WriteTarget};
#[cfg(doc)]
use crate::document::EditError;
use crate::document::{BlockLocator, PageDocument};
use crate::domain::{BlockSelector, BlockView, Read, ReadMeta};
use crate::engine::snapshot::{collapsed_value, COLLAPSED_PROPERTY};
use crate::engine::Graph;

impl Graph {
    /// Set the block's `collapsed::` property to `collapsed`, and answer the block as the
    /// committed revision holds it.
    ///
    /// `collapsed::` is an ordinary property line, exactly the one a user writes by hand in the
    /// editor: `true` folds the block, `false` unfolds it. The edit is a
    /// [`Self::upsert_block_property`], not a separate document rule — the key is spelled the
    /// way the file already spells it, the value replaces the line the key has or gains one after
    /// the block's other properties, and every other byte of the page is copied unchanged. Because
    /// [`PageDocument::update_block_text`] preserves `collapsed::` the way it preserves `id::`,
    /// editing the block's text afterwards never loses the folded state.
    ///
    /// This method exists so a caller never spells the key: the folded state is a property, and
    /// this is the named door to it. Everything it refuses, and everything it answers, is
    /// [`Self::upsert_block_property`]'s.
    pub fn set_block_collapsed(
        &mut self,
        selector: &BlockSelector,
        collapsed: bool,
    ) -> Result<Read<BlockView>, WriteError> {
        self.upsert_block_property(selector, COLLAPSED_PROPERTY, collapsed_value(collapsed))
    }

    /// Replace the text of the block the selector names.
    ///
    /// `text` is the block's own content, copied verbatim: a `\n` starts a continuation line, which
    /// is written indented to the block's content column so the parser reads it back as the same
    /// block. The block keeps its marker bytes (`-`, `*`, or `+`), its `id::` and `collapsed::`
    /// lines, and its children; every byte outside its own lines is copied unchanged, so the BOM,
    /// each line's ending, trailing blank lines, and a missing final newline all survive.
    ///
    /// # Why the target is a [`BlockSelector`]
    ///
    /// Both identity kinds name one block of one page, and a caller edits by the address it read: a
    /// durable id when the block states one, the ephemeral id the projection minted when it does not
    /// (ADR-0012). An ephemeral id resolves to one index row, and that row carries the child-index
    /// path to edit by and the fingerprint of the bytes it was projected from. The write compares
    /// that fingerprint with the file under the write lock before it computes any edit, so an id
    /// whose page moved on is refused with [`WriteError::StaleVersion`] and no byte is written; an
    /// id no row holds is [`WriteError::EphemeralNotFound`]. Neither case can land on a neighbour,
    /// which is the whole point of the identity being random rather than positional.
    ///
    /// # Freshness
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives. A durable
    /// target consults the index only as a map from an id to a page, and the answer it gives is
    /// checked against the file step 5 reads, so an outdated row can send the edit to the right file
    /// but cannot invent a block: the edit refuses when the file does not state the id. The scan
    /// appears in step 2 as a fallback for the one question the index cannot answer at all — an id no
    /// row names. An ephemeral target is one equality on the unique ephemeral index and nothing else:
    /// a miss is a refusal, never a scan, because no scan could produce a row that holds the value.
    /// Its coordinates are then checked against the bytes the edit transforms, which is what makes
    /// them trustworthy (ADR-0012).
    ///
    /// # Order and failure
    ///
    /// The steps are [`Graph::append_block`]'s, and only four of them differ:
    ///
    /// 1. **Take the write lock**, for the same reason: a scan and a file replacement must not
    ///    interleave with another writer.
    /// 2. **Resolve the selector to its page.** A durable id goes through the index: a hit is one
    ///    query and no file read, a miss pays for one `ScanMode::ReadEveryFile` scan and asks again,
    ///    and a second miss is [`WriteError::BlockNotFound`], by which time every file has been read
    ///    and projected so the id really is absent from the graph. That order keeps the common path
    ///    free of the scan without giving up correctness on the rare one. A hit that names more than
    ///    one page is [`WriteError::AmbiguousBlockId`], refused before any file is read: one file
    ///    cannot settle which block was meant. A hit naming one page twice is *not* refused here,
    ///    because that page's file is the authority on whether it still does, and step 4 refuses it
    ///    there. An ephemeral id is one seek on the unique `blocks.ephemeral_id` index: a miss is
    ///    [`WriteError::EphemeralNotFound`] with no scan behind it, and a hit contributes its page,
    ///    its child-index path, and the fingerprint of the bytes that row describes.
    /// 3. **Find that page's file and read it.** The name comes from the resolution above, so this is
    ///    the same one query plus one stat as the append; discovery answers only when the row's path
    ///    no longer holds the page. A page the index names but discovery no longer finds is
    ///    [`WriteError::PageNotFound`], reported separately from [`WriteError::BlockNotFound`]: "the
    ///    index knows a block but not its page's file" and "no block names that address" are
    ///    different facts.
    /// 4. **Check the bytes, then apply the pure edit.** For an ephemeral target the fingerprint of
    ///    the row's bytes is compared with the file first, and a mismatch is
    ///    [`WriteError::StaleVersion`] before any edit is computed (ADR-0012). A refused edit leaves
    ///    the file untouched. For a durable write the refusals worth naming are
    ///    [`EditError::BlockNotFound`] — the file no longer states the id — and
    ///    [`EditError::AmbiguousBlockId`], which means it states the id twice: a corrupt file rather
    ///    than a caller mistake, because a durable id names exactly one block (ADR-0005) and editing
    ///    either candidate would change an arbitrary one of them. Both kinds arrive as
    ///    [`WriteError::Edit`].
    /// 5. **Replace the file atomically**, the first moment anything changes.
    /// 6. **Re-observe, re-read, and re-parse**, so the index is given the bytes that are actually
    ///    there rather than the candidate from step 4.
    /// 7. **Replace the whole page in one transaction**, recording the baseline of the observation
    ///    taken before the read.
    ///
    /// A failure in steps 1 to 4 leaves the graph exactly as it was; a failure in steps 6 or 7
    /// leaves the new file on disk with the old index, reported as
    /// [`WriteError::ReReadAfterWrite`] or [`WriteError::IndexAfterWrite`] and repaired by the next
    /// scan. The file is never rolled back.
    ///
    /// # What it answers
    ///
    /// The block as the committed revision holds it. A durable write addresses it by the id the call
    /// was asked with; an ephemeral write by the position it edited, whether or not the block states
    /// an `id::`. Step 7 just projected the page, so the row is there; a missing one means the
    /// stored projection contradicts itself, and that is reported as [`WriteError::Index`] rather
    /// than as a block that vanished. `is_stale` reports whether an earlier scan failed: this page is
    /// fresh because step 7 committed it, and no other page was refreshed by this call.
    pub fn update_block(
        &mut self,
        selector: &BlockSelector,
        text: &str,
    ) -> Result<Read<BlockView>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_selector_write(&[selector])?;

        // Step 2: resolve the selector to its page and address. A durable id is resolved through
        // the index, with its scan fallback; an ephemeral id is one equality on the unique
        // `blocks.ephemeral_id` index, and a miss there is a refusal rather than a scan, because no
        // scan could produce a row holding an id that no row holds.
        let target = self.write_target(selector)?;

        // Step 3: resolve the page name the index stored to the file that holds it, with the same one
        // query the append uses: the row already names the file, and discovery is only the answer for
        // a row whose file moved.
        let file = self.locate_page(target.page())?;

        // Step 4: the edit is applied to the bytes on disk and stays a pure transformation until
        // it is accepted. Before the edit is computed, an ephemeral
        // target's fingerprint is compared with the bytes just read: its path is a coordinate in
        // those bytes and nothing else, so a file another program changed is refused here, with zero
        // bytes written (ADR-0012). A durable write needs no such check, because the id it edits by
        // is stated in the bytes the edit reads.
        let original = file.read_text()?;
        check_ephemeral_freshness(file.name(), &original, target.page_fingerprint())?;
        let candidate = PageDocument::parse(&original).update_block_text(target.locator(), text)?;

        // Steps 5 to 7 are the shared single-page commit. The caller still holds the write lock.
        let committed = self.commit_page_edit(&file, candidate)?;

        // The answer is the row the replacement just committed, not the block this call parsed: the
        // preorder, depth, text, and selector are facts about what was stored, and re-deriving them
        // from the document would be a second opinion that could disagree with the projection. The
        // row is there because step 7 replaced this page; a missing one means the stored projection
        // contradicts itself.
        //
        // Every write of this shape reprojects the whole page, so the answer is a new row with a new
        // ephemeral id: an update keeps the block's preorder and a durable id it does not edit, and
        // both are addresses the edit cannot invalidate, which is why they can be read back after it.
        let view = self.answer_block(file.name(), &target)?;

        // Stale means an earlier scan failed, which is all this write can say about the graph: step 7
        // committed this page, so its row matches its file, and no other page was re-established by
        // this call.
        Ok(Read::new(view, self.read_meta(committed, self.is_stale())))
    }

    /// Set one `key:: value` property of the block the selector names, and answer the block as the
    /// committed revision holds it.
    ///
    /// The edit is [`PageDocument::upsert_block_property_text`]: a key the block already states —
    /// compared by normalized key — has its line replaced with the written spelling kept, a key it
    /// does not state gains one line after the block's other properties, and every other byte of
    /// the page is copied unchanged. `id::` is an ordinary key here, so this is also ADR-0009's
    /// explicit path for assigning a durable id, and removing the key again with
    /// [`Self::remove_block_property`] breaks the `((...))` references that pointed at the block.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Self::update_block`]'s, and the refusals are the same ones plus one: the
    /// edit adds [`EditError::InvalidPropertyKey`] for a key no property line can carry, and
    /// [`EditError::PropertyNotFound`] is [`Self::remove_block_property`]'s answer, not this one's —
    /// an upsert that finds no line has nothing to refuse.
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives, and it
    /// reprojects only the page it rewrote.
    pub fn upsert_block_property(
        &mut self,
        selector: &BlockSelector,
        key: &str,
        value: &str,
    ) -> Result<Read<BlockView>, WriteError> {
        self.write_block_property(selector, false, |document, locator| {
            Ok(document.upsert_block_property_text(locator, key, value)?)
        })
        .map(|(view, meta)| Read::new(view, meta))
    }

    /// Remove the `key:: value` line the block the selector states, and answer the block as the
    /// committed revision holds it.
    ///
    /// The edit is [`PageDocument::remove_block_property_text`]: the line is found by normalized
    /// key, only that line is cut, and a block that states no property with the key is refused with
    /// [`WriteError::Edit`] carrying [`EditError::PropertyNotFound`] — removing what is not there is
    /// a caller mistake, not a no-op, because the caller's belief about the block is what the
    /// refusal corrects.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Self::update_block`]'s; see [`Self::upsert_block_property`] for what the two
    /// property writes share. This write does not scan the graph, for the reason
    /// [`Graph::append_block`] gives, and it reprojects only the page it rewrote.
    pub fn remove_block_property(
        &mut self,
        selector: &BlockSelector,
        key: &str,
    ) -> Result<Read<BlockView>, WriteError> {
        self.write_block_property(selector, false, |document, locator| {
            Ok(document.remove_block_property_text(locator, key)?)
        })
        .map(|(view, meta)| Read::new(view, meta))
    }

    /// The pipeline [`Self::upsert_block_property`], [`Self::remove_block_property`], and
    /// [`Self::set_block_collapsed`] share: [`Self::update_block`]'s single-page path with the edit
    /// swapped out, answering the edited block and the metadata of the revision it was committed in.
    ///
    /// The answer is looked up by a position resolved from the bytes before the edit — the row's
    /// preorder for an ephemeral selector, the document's preorder path for a durable one, see the
    /// resolution below for why not by the selector's id. The lookup cannot miss: the edit ran
    /// against bytes that pass the fingerprint gate, and a property line added, replaced, or removed
    /// around a block does not move it, so the row the replacement commits still holds that position.
    pub(super) fn write_block_property(
        &mut self,
        selector: &BlockSelector,
        skip_unchanged: bool,
        edit: impl FnOnce(&PageDocument, BlockLocator<'_>) -> Result<String, WriteError>,
    ) -> Result<(BlockView, ReadMeta), WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed, for
        // the reason the other writes take it — and one of its own, since the fingerprint gate below
        // was added: a check that the bytes are the ones a row describes is only a check if nothing
        // can replace those bytes between the check and the write, and another writer in this process
        // or in a sibling one is exactly that. The lock is therefore not an optimisation here; the
        // gate's premise depends on it.
        let _lock = self.begin_selector_write(&[selector])?;

        // Steps 2 to 5 are `update_block`'s, including the durable lookup through the index with its
        // scan fallback, the ephemeral id's one index equality, and the fingerprint gate that refuses
        // bytes the ephemeral row was not projected from.
        let target = self.write_target(selector)?;
        let file = self.locate_page(target.page())?;
        let original = file.read_text()?;
        check_ephemeral_freshness(file.name(), &original, target.page_fingerprint())?;
        let document = PageDocument::parse(&original);
        // The edit is addressed by the locator, and the answer is read back by a position the edit
        // cannot invalidate: a property write may replace the very `id::` value a durable selector
        // carried, so an id that the edit itself invalidates is no address, while the block's place
        // in the structure survives a line being added, replaced, or removed around it.
        //
        // An ephemeral target already knows that place — the row it resolved to carries the preorder
        // — and a durable one is resolved here by the same rule every edit resolves it with, so the
        // errors are the edit's own.
        let (answer, candidate) = match &target {
            WriteTarget::Ephemeral(located) => (
                AnswerAddress::Preorder(located.preorder),
                edit(&document, BlockLocator::Indices(&located.child_path))?,
            ),
            WriteTarget::Durable { id, .. } => {
                let path = document.block_path(BlockLocator::Id(id.as_str()))?;
                let candidate = edit(&document, BlockLocator::Id(id.as_str()))?;
                (AnswerAddress::Path(path), candidate)
            }
        };
        if skip_unchanged && candidate == original {
            let view =
                super::identity::observed_durable_view(file.name(), &document, target.locator())?;
            let meta = self.read_meta(self.index.metadata()?, self.is_stale());
            return Ok((view, meta));
        }
        // Steps 5 to 7 are the shared single-page commit. The caller still holds the write lock.
        let committed = self.commit_page_edit(&file, candidate)?;

        // The answer is the row the replacement just committed, read by the position resolved before
        // the edit, exactly as `update_block` reads its answer back. Its selector is whatever the row
        // states now: a property write may have added, replaced, or removed the `id::` line, and a
        // block that gains one answers a durable selector while a block that loses one answers the
        // ephemeral id this same replacement minted for it (ADR-0012).
        let view = self.answer_view(file.name(), answer)?;

        let meta = self.read_meta(committed, self.is_stale());
        Ok((view, meta))
    }
}
