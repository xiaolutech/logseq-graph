use super::error::{answer_after_write, WriteError};
use super::moves::MoveTarget;
use super::target::check_ephemeral_freshness;
#[cfg(doc)]
use crate::document::EditError;
use crate::document::{BlockLocator, MoveTarget as DocumentMoveTarget, PageDocument};
#[cfg(doc)]
use crate::domain::{page_name_key, ReadMeta};
use crate::domain::{BlockSelector, PageMetadata, Read};
use crate::engine::Graph;
use crate::index::IndexError;

impl Graph {
    /// Append one top-level block to `page` and answer the page's new metadata.
    ///
    /// `text` becomes the content of one new block; it is copied verbatim, so it must fit on one
    /// source line. `page` is matched by [`page_name_key`], the same folding every page read uses,
    /// so `foo` appends to the file that holds `Foo.md` and writes no second file.
    ///
    /// # Why this answers [`PageMetadata`] and not the new block's address
    ///
    /// The appended block has no `id::`, and this write does not answer a selector for it — a
    /// durable id would have to be invented, and an ephemeral id belongs to the row the projection
    /// minted it on (ADR-0012). What this write promises is the page: it exists, it has this many
    /// blocks, and this is the revision the write committed. A caller that needs the block reads
    /// [`Graph::page_blocks`] at the returned revision, where the last block's
    /// [`selector`](crate::domain::BlockView::selector) names it — one read, not a second write.
    ///
    /// # Freshness
    ///
    /// This write does not scan the graph, and nothing it does depends on the index being current:
    /// the page is resolved by looking at the files, and the edit is applied to the bytes it reads
    /// from the file it replaces. What it answers is therefore fresh for this page and for no other,
    /// which is why [`ReadMeta::is_stale`] reports whether an earlier scan failed here rather than
    /// claiming the whole model is current. A caller that needs the rest of the graph refreshed
    /// first reads once with [`Freshness::VerifyContent`] and pays for it explicitly.
    ///
    /// # Order and failure
    ///
    /// The steps are the orchestration ADR-0004 fixes, and each one exists for a reason:
    ///
    /// 1. **Take the write lock**, in the graph's `.git` when it is a repository and next to the index
    ///    file otherwise. A scan another process runs and this file replacement must not interleave,
    ///    or that scan could project bytes the next write replaces, and the baseline recorded for the
    ///    projection would describe a file that no longer exists.
    /// 2. **Find the file by page name.** The index stores the file each page was projected from, so
    ///    this is one query and one stat; a page with no row, or a row whose file moved, falls back to
    ///    one discovery. A miss there is [`WriteError::PageNotFound`]; this never creates a page.
    /// 3. **Read and parse the file**, so the edit is applied to the bytes on disk rather than to
    ///    anything the index believes.
    /// 4. **Apply the pure edit.** A refused edit leaves the file untouched.
    /// 5. **Replace the file atomically**, which is the first moment anything changes.
    /// 6. **Re-observe, re-read, and re-parse.** The index input is this fresh parse, not the
    ///    candidate from step 4: the candidate is what this process meant to write, and the file is
    ///    what is actually there.
    /// 7. **Replace the whole page in one transaction**, recording the baseline of the observation
    ///    taken before the read.
    ///
    /// A failure in steps 1 to 4 leaves the graph exactly as it was. A failure in steps 6 or 7 leaves
    /// the new file on disk with the old index, which is reported as [`WriteError::ReReadAfterWrite`]
    /// or [`WriteError::IndexAfterWrite`] and repaired by the next scan. The file is never rolled
    /// back.
    ///
    /// [`BlockId`]: crate::domain::BlockId
    /// [`Freshness::VerifyContent`]: crate::engine::Freshness::VerifyContent
    pub fn append_block(
        &mut self,
        page: &str,
        text: &str,
    ) -> Result<Read<PageMetadata>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_write()?;

        // No scan here. The edit below reads this file's bytes and transforms them; it never derives
        // output from an index row, so no page in the index can be stale enough to lose data. A
        // caller that wants the whole graph refreshed first asks for that read itself.

        // Step 2: resolve the page to the file that holds it. The index answers with the path of the
        // file the page was last projected from, and discovery answers for a page it has no row for
        // yet or whose file moved.
        let target = self.locate_page(page)?;

        // Steps 3 and 4: the edit is applied to the bytes on disk and stays a pure transformation
        // until the file is replaced.
        let original = target.read_text()?;
        let candidate = PageDocument::parse(&original).append_block(text)?;

        // Steps 5 to 7 are the shared single-page commit. The caller still holds the write lock.
        let committed = self.commit_page_edit(&target, candidate)?;

        // The metadata comes from the projection rather than from the parsed document: the block
        // count is a fact about what was stored, and re-deriving it here would be a second opinion
        // that could disagree with the rows. The replacement just committed this page, so the row is
        // there; a missing one means the stored projection contradicts itself.
        let metadata = self
            .index
            .page_metadata(target.name())
            .map_err(|cause| answer_after_write(target.name(), target.path(), cause))?
            .ok_or_else(|| WriteError::IndexAfterWrite {
                page: target.name().to_owned(),
                path: target.path().to_owned(),
                cause: IndexError::CorruptRow {
                    column: "pages.name",
                    value: target.name().to_owned(),
                },
            })?;

        // Stale means an earlier scan failed, which is all this write can say about the graph: step 7
        // committed this page, so its rows match its file, and no other page was re-established by
        // this call.
        Ok(Read::new(
            metadata,
            self.read_meta(committed, self.is_stale()),
        ))
    }

    /// Insert one new block whose content is `text` at the position `target` names, and answer the
    /// block that was created.
    ///
    /// This is the one write that names a block that did not exist, so it answers the block instead
    /// of the page's metadata: the caller asked where to put a block and needs to keep editing the
    /// block it just put there, which is exactly what every other write answers with. The answer
    /// follows ADR-0009's rule for newly minted blocks. A `text` that states `id:: <uuid>` makes
    /// the new block durable, and the answer is a durable selector for the id the text declared;
    /// any other text creates an ephemeral block, and the answer is the ephemeral id the committed
    /// projection minted for it (ADR-0012), which names that block until the page's bytes change.
    ///
    /// # Freshness
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Graph::move_block`]'s single-page path, because an insertion lives where its
    /// anchor lives and can never span two files — there is no cross-page shape to recover, and
    /// unlike a move the anchor's own lines stay where they are, so the document edit cannot refuse
    /// a target inside a moved subtree either.
    ///
    /// 1. **Take the write lock**, which serialises this write with every other writer.
    /// 2. **Resolve the anchor to its page and address**, exactly as [`Graph::move_block`]'s step 2
    ///    resolves the target end: a durable id through the index with the scan fallback, an
    ///    ephemeral id through the unique index that holds it, together with the fingerprint of the
    ///    bytes its row describes.
    /// 3. **Find that page's file and read it.**
    /// 4. **Apply the pure edit.** [`PageDocument::insert_block_text`] returns the candidate text
    ///    and the preorder path the new block holds in it, parsed back out of the candidate rather
    ///    than derived from line numbers, so the address is the one the next read of this file
    ///    answers. [`EditError::EmptyContent`] and [`EditError::MultilineContent`] refuse the edit
    ///    like an append's, and a missing anchor is [`EditError::MoveTargetNotFound`], which
    ///    arrives as [`WriteError::Edit`].
    /// 5. **Replace the file atomically**, the first moment anything changes.
    /// 6. **Re-observe, re-read, and re-parse**, so the index is given the bytes that are actually
    ///    there rather than the candidate from step 4.
    /// 7. **Replace the whole page in one transaction**, recording the baseline of the observation.
    ///
    /// A failure in steps 1 to 4 leaves the graph exactly as it was; a failure in steps 6 or 7
    /// leaves the new file on disk with the old index, reported as
    /// [`WriteError::ReReadAfterWrite`] or [`WriteError::IndexAfterWrite`] and repaired by the next
    /// scan. The file is never rolled back.
    ///
    /// # What it answers
    ///
    /// The block the write created, as the committed revision holds it. The durable or ephemeral
    /// decision is read from the projection, not from the text the call was asked with: a durable
    /// answer is the id the new block's own row carries, and an ephemeral one binds the position
    /// the projection stored, which is the address an immediate update or remove of the new block
    /// resolves against. Step 7 just projected the page, so the row is there; a missing one means
    /// the stored projection contradicts itself, and that is reported as [`WriteError::Index`].
    /// `is_stale` reports whether an earlier scan failed: this page is fresh because step 7
    /// committed it, and no other page was refreshed by this call.
    pub fn insert_block(
        &mut self,
        target: MoveTarget,
        text: &str,
    ) -> Result<Read<BlockSelector>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_selector_write(&[target.anchor()])?;

        // Step 2: the anchor is resolved the way a move's target end is, so a durable id that only
        // an unprojected file states pays for one scan rather than being reported as missing, and an
        // ephemeral anchor resolves through the index to the path its row names.
        let anchor = self.resolve_anchor(target.anchor())?;

        // Step 3: resolve the page name the index stored to the file that holds it.
        let file = self.locate_page(&anchor.page)?;

        // Step 4: the edit is applied to the bytes on disk and stays a pure transformation until
        // the file is replaced. An ephemeral anchor's fingerprint is checked first, so a position
        // read from a projection the file no longer matches cannot place the new block beside a
        // neighbour (ADR-0012). `as_document_target` borrows the resolved locator, so the edit can
        // fail and the caller's target is still theirs to retry with.
        let original = file.read_text()?;
        check_ephemeral_freshness(file.name(), &original, anchor.page_fingerprint)?;
        let (candidate, new_path) = PageDocument::parse(&original)
            .insert_block_text(target.as_document_target(anchor.locator.as_locator()), text)?;

        // Steps 5 to 7, and the answer read back from the projection, are the single-page
        // pipeline [`Self::commit_page_edit`] and [`Self::answer_selector`] share with the batch
        // insert and the page prepend.
        let committed = self.commit_page_edit(&file, candidate)?;
        let selector = self.answer_selector(file.name(), &new_path)?;

        Ok(Read::new(
            selector,
            self.read_meta(committed, self.is_stale()),
        ))
    }

    /// Insert several new blocks whose contents are `texts`, in order, at the position `target`
    /// names, and answer every block that was created, in the order `texts` gives.
    ///
    /// The first block lands exactly where [`Self::insert_block`] would put one, and each later
    /// block lands immediately after the previous one, so the run comes out as siblings at the
    /// indentation level `target` states. The answer follows ADR-0009's rule for newly minted
    /// blocks, per block: a text that states `id:: <uuid>` answers that id, any other text answers
    /// the ephemeral id the projection this write committed minted for it (ADR-0012).
    ///
    /// This is one page replacement, not one per block: pasting a run of N blocks one `insert`
    /// at a time would pay N locks, N reads, N parses, N file replacements, and N projections for
    /// what is one edit's worth of bytes. Here the N edits are applied to the bytes one call read,
    /// and the file is replaced, re-observed, and reprojected once. Every block in the run is
    /// validated before the first line is inserted, so one illegal text refuses the whole batch
    /// and leaves the graph byte-identical — a batch contract, not a loop's.
    ///
    /// An empty `texts` is a success that writes nothing: the call takes the write lock and
    /// resolves the anchor like any insert, but reads no file and replaces none, because there is
    /// no edit to apply. This keeps the code path one pipeline instead of a special case before
    /// it, and the contract the caller relies on — the graph is exactly as it was, and the answer
    /// is an empty run at this revision — is the same either way.
    ///
    /// # Freshness
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Self::insert_block`]'s, with step 4 applying every edit of the run:
    ///
    /// 1. **Take the write lock.**
    /// 2. **Resolve the anchor to its page**, exactly as [`Self::insert_block`]'s step 2 does.
    /// 3. **Find that page's file and read it.** Skipped when `texts` is empty.
    /// 4. **Apply the pure edits** with [`PageDocument::insert_blocks_text`], which inserts each
    ///    block after the previous one and answers every path from the parse of the text the run
    ///    assembled. All of `texts` is validated first, so a refusal here leaves the graph as it
    ///    was.
    /// 5. **Replace the file atomically**, once, the first moment anything changes.
    /// 6. **Re-observe, re-read, and re-parse**, so the index is given the bytes that are actually
    ///    there rather than the candidate from step 4.
    /// 7. **Replace the whole page in one transaction**, recording the baseline of the observation.
    ///
    /// A failure in steps 1 to 4 leaves the graph exactly as it was; a failure in steps 6 or 7
    /// leaves the new file on disk with the old index, reported as
    /// [`WriteError::ReReadAfterWrite`] or [`WriteError::IndexAfterWrite`] and repaired by the
    /// next scan. The file is never rolled back.
    ///
    /// # What it answers
    ///
    /// One selector per block of the run, in `texts`' order, each read from the projection the
    /// replacement just committed — see `Self::answer_selector`. The revision the result reports
    /// is this write's own, and one commit advanced it exactly once, which is what makes the run one
    /// replacement rather than N. The rows are there because step 7 replaced this page; a missing one
    /// means the stored projection contradicts itself, and that is reported as [`WriteError::Index`].
    /// `is_stale` reports whether an earlier scan failed: this page is fresh because step 7
    /// committed it, and no other page was refreshed by this call.
    pub fn insert_batch(
        &mut self,
        target: MoveTarget,
        texts: &[&str],
    ) -> Result<Read<Vec<BlockSelector>>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_selector_write(&[target.anchor()])?;

        // Step 2: the anchor is resolved exactly as a single insert resolves it, so an empty batch
        // still refuses a target nothing can anchor to rather than answering a run it never placed.
        let anchor = self.resolve_anchor(target.anchor())?;

        // Step 3: resolve the page name the index stored to the file that holds it, and read it.
        // An empty batch has no edit to apply, so the file is neither read nor replaced; the run
        // answers at the revision currently committed.
        if texts.is_empty() {
            let meta = self.index.metadata()?;
            return Ok(Read::new(Vec::new(), self.read_meta(meta, self.is_stale())));
        }
        let file = self.locate_page(&anchor.page)?;
        let original = file.read_text()?;
        check_ephemeral_freshness(file.name(), &original, anchor.page_fingerprint)?;

        // Step 4: every edit of the run, applied to the bytes this one read. All texts are
        // validated before the first line is inserted, so a refusal here leaves the file alone.
        let (candidate, paths) = PageDocument::parse(&original).insert_blocks_text(
            target.as_document_target(anchor.locator.as_locator()),
            texts,
        )?;

        // Steps 5 to 7 run once for the whole run, and the answer is one selector per block.
        let committed = self.commit_page_edit(&file, candidate)?;
        let mut selectors = Vec::with_capacity(paths.len());
        for path in &paths {
            selectors.push(self.answer_selector(file.name(), path)?);
        }

        Ok(Read::new(
            selectors,
            self.read_meta(committed, self.is_stale()),
        ))
    }

    /// Insert one new block whose content is `text` before the page's first block, and answer the
    /// block that was created.
    ///
    /// A page with at least one block gets exactly what [`Self::insert_block`] gives for a
    /// `Before` target on its first top-level block; a page with none gets the block as its first
    /// and only block, placed by [`PageDocument::append_block`] — on an empty page the end of the
    /// last line *is* the front. The answer follows ADR-0009's rule for newly minted blocks, like
    /// every write that names a block that did not exist: a `text` that states `id:: <uuid>` makes
    /// the new block durable, any other text answers the ephemeral id the committed projection
    /// minted for it (ADR-0012), which names that block until the page's bytes change.
    ///
    /// # Freshness
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Self::insert_block`]'s. The one branch is in step 4: the page's first block
    /// is found in the bytes the call just read, and the edit goes through
    /// [`PageDocument::insert_block_text`] with a `Before` target on it, or through
    /// [`PageDocument::append_block`] when the page holds no block to anchor on. Both are the same
    /// pure-edit step, judged by the same refusals, committed by the same steps 5 to 7, and
    /// answered from the same projection, so the two shapes cannot drift apart.
    ///
    /// # What it answers
    ///
    /// The block the write created, as the committed revision holds it — see
    /// [`Self::insert_block`]'s answer for the durable or ephemeral decision and why the answer is
    /// the block and not the page: the caller asked where to put a block and needs to keep editing
    /// it. `is_stale` reports whether an earlier scan failed; this page is fresh because step 7
    /// committed it.
    pub fn prepend_block_in_page(
        &mut self,
        page: &str,
        text: &str,
    ) -> Result<Read<BlockSelector>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_write()?;

        // Step 2: resolve the page to the file that holds it, exactly as `append_block` resolves
        // the page it appends to — this write names a page, not a block to anchor on.
        let target = self.locate_page(page)?;

        // Step 3: read the bytes the edit transforms, and find the first block in them. The top of
        // the preorder is the first top-level block, so its path is `[0]`; a page with no block
        // has no anchor, and takes the append path below.
        let original = target.read_text()?;
        let document = PageDocument::parse(&original);

        // Step 4: apply the pure edit. A page with a block delegates the position to the document
        // layer's own insert — the engine never spells out where "before the first block" is in
        // bytes; a page without one appends, which on an empty page is the front.
        let (candidate, new_path) = if document.blocks().is_empty() {
            let candidate = document.append_block(text)?;
            // The appended block is the page's first and only one.
            (candidate, vec![0])
        } else {
            document
                .insert_block_text(DocumentMoveTarget::Before(BlockLocator::Path(&[0])), text)?
        };

        // Steps 5 to 7, and the answer, are the pipeline [`Self::commit_page_edit`] and
        // [`Self::answer_selector`] share with the single insert and the batch.
        let committed = self.commit_page_edit(&target, candidate)?;
        let selector = self.answer_selector(target.name(), &new_path)?;

        Ok(Read::new(
            selector,
            self.read_meta(committed, self.is_stale()),
        ))
    }
}
