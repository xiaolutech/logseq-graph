//! One commit against one page: an ordered batch of positional operations, applied to one candidate
//! document and landed as one file replacement and one whole-page projection (ADR-0004,
//! ADR-0011).
//!
//! This module is [`Graph::commit_page`] and the mapping from one [`PageOperation`] to the one
//! [`PageDocument`] edit that performs it. It orchestrates and holds no Markdown rule of its own
//! (ADR-0003): every refusal, every byte, and every position comes from the document layer, and the
//! file, the re-read, and the projection come from the pipeline [`Graph::commit_page_edit_observed`]
//! shares with the other writes.
//!
//! The decision this module states is the *order*: the base version is compared before anything is
//! applied, every operation is applied to bytes that exist only in memory, and the file is replaced
//! once after the last of them succeeded. A commit therefore cannot leave a prefix of itself on
//! disk, and a caller that loses the version race loses the whole commit rather than part of it.

use super::snapshot::{collapsed_value, page_version, snapshot_from_text, COLLAPSED_PROPERTY};
use super::{Graph, WriteError};
use crate::document::{BlockLocator, EditError, MoveTarget, PageDocument};
use crate::domain::{BatchMoveTarget, PageCommit, PageCommitResult, PageOperation, Read};

impl Graph {
    /// Apply an ordered batch of operations to one page, file first and index second (ADR-0004).
    ///
    /// The operations run in array order against one candidate document, each one resolved against
    /// the candidate the previous ones left — so an operation can address a block an earlier
    /// operation of the same commit minted or moved. The result is written, re-read, re-parsed and
    /// projected once: one file replacement, one whole-page projection, one revision advance, for
    /// the whole batch rather than one of each per operation.
    ///
    /// A commit whose operations are all legal but whose page changed since its paths were computed
    /// is refused, and so is a commit with one illegal operation; neither writes a byte nor changes
    /// a row. See [`WriteError::StaleVersion`] and [`WriteError::Operation`].
    ///
    /// # Freshness
    ///
    /// With a matching configuration generation, this write does not scan the graph, for the
    /// reason [`Graph::append_block`] gives: the bytes it
    /// transforms are the bytes it just read from the file it replaces. Its staleness guard is
    /// page-scoped and reads the file, not the index, so a stale row cannot make the guard pass and a
    /// change to another page cannot make it fail.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Graph::append_block`]'s, with the batch's guard at step 3 and its loop at
    /// step 4:
    ///
    /// 1. **Take the write lock.** The version check, the operations, and the replacement have to
    ///    describe one uninterrupted run, or another writer could change the page between the check
    ///    and the write.
    /// 2. **Resolve the page name to its file**, by the same rule as every write: the index's stored
    ///    path, checked against the file it still denotes, with one discovery behind a miss.
    /// 3. **Read the bytes and compare their version with the commit's base version.** The comparison
    ///    is against the page's own source bytes under the lock, never against the SQLite revision,
    ///    because another program can edit this file without touching the index. A mismatch is
    ///    [`WriteError::StaleVersion`], carrying a snapshot of the bytes just read so the caller can
    ///    recompute its paths: nothing was written, so retrying against the current version is
    ///    always safe. An empty commit passes the same guard.
    /// 4. **Apply the operations in order to a candidate document.** Each operation re-parses the
    ///    candidate before it resolves its path, which is what makes "paths are resolved against the
    ///    document as it stands when the operation runs" structural rather than a rule to remember;
    ///    the price is one parse per operation. A refusal is [`WriteError::Operation`], naming the
    ///    operation's position and variant: the candidate is thrown away and the file is never
    ///    touched, so a failed batch leaves no prefix behind.
    /// 5. **Replace the file once** — after the whole batch succeeded, and not at all when the batch
    ///    is empty.
    /// 6. **Re-observe, re-read, and re-parse** the file, so the index is given the bytes that are
    ///    actually there rather than the candidate from step 4.
    /// 7. **Replace the whole page in one transaction**, recording the baseline of the observation.
    ///    This is the only revision this call advances, however many operations it applied.
    ///
    /// A failure in steps 1 to 4 leaves the graph exactly as it was; a failure in steps 6 or 7 leaves
    /// the new file on disk with the old index, reported as [`WriteError::ReReadAfterWrite`] or
    /// [`WriteError::IndexAfterWrite`] and repaired by the next scan. The file is never rolled back.
    ///
    /// # What it answers
    ///
    /// [`PageCommitResult::snapshot`] describes the page the re-read bytes hold, so it is the page the
    /// next read answers rather than a second opinion about the candidate, and
    /// [`PageCommitResult::applied`] is the number of operations applied. The metadata's revision is
    /// the one step 7 committed; an empty commit writes nothing and reports the revision that was
    /// already committed.
    pub fn commit_page(
        &mut self,
        commit: &PageCommit,
    ) -> Result<Read<PageCommitResult>, WriteError> {
        // Step 1: serialise with every other writer before anything is observed, because the version
        // check below is only worth anything if the bytes cannot change between it and the write.
        let _lock = self.begin_write()?;

        // Step 2: resolve the page name to the file that holds it.
        let file = self.locate_page(commit.page())?;

        // Step 3: the guard. The version is computed from the bytes in hand, so this reads the page's
        // authority and not a derived copy of it; a mismatch refuses the whole commit while nothing
        // has been written and no row has been touched.
        let original = file.read_text()?;
        if page_version(&original) != *commit.base_version() {
            return Err(WriteError::StaleVersion {
                page: file.name().to_owned(),
                base_version: commit.base_version().clone(),
                current: Box::new(snapshot_from_text(file.name(), &original)),
            });
        }

        // An empty batch has nothing to apply and nothing to replace. It is still a success with an
        // answer — the page as these bytes describe it — so a caller's retry loop stays one shape.
        if commit.operations().is_empty() {
            let meta = self.read_meta(self.index.metadata()?, self.is_stale());
            return Ok(Read::new(
                PageCommitResult::new(snapshot_from_text(file.name(), &original), 0),
                meta,
            ));
        }

        // Step 4: apply every operation to the in-memory candidate. Each one sees the document the
        // previous one produced, so its path is resolved against that state and not against the bytes
        // the commit was computed from.
        let mut candidate = original;
        for (index, operation) in commit.operations().iter().enumerate() {
            let next = {
                let document = PageDocument::parse(&candidate);
                apply_operation(&document, operation)
            };
            candidate = next.map_err(|cause| WriteError::Operation {
                page: file.name().to_owned(),
                index,
                operation: operation.name(),
                cause,
            })?;
        }

        // Steps 5 to 7: one replacement, one re-read, one whole-page projection. The pipeline also
        // answers the text it observed, so the snapshot below describes exactly the bytes the index
        // was given — and that text is parsed twice, once by the pipeline for the projection and
        // once here for the answer. The second parse is the price of the answer being the bytes
        // rather than a second opinion about the candidate; a batch pays it once, however many
        // operations it applied.
        let (committed, fresh_text) = self.commit_page_edit_observed(&file, candidate)?;

        Ok(Read::new(
            PageCommitResult::new(
                snapshot_from_text(file.name(), &fresh_text),
                commit.operations().len(),
            ),
            self.read_meta(committed, self.is_stale()),
        ))
    }
}

/// Apply one operation to the candidate document and answer the next candidate.
///
/// Each arm is one document edit and nothing else: the operation states *what* changes, and the edit
/// owns the bytes, the positions, and the refusals. The mapping is total on purpose — a new
/// [`PageOperation`] variant stops this from compiling until someone decides which edit it means.
///
/// A `MoveBlock` names two paths, the moved block and its anchor, and a miss on either arrives as
/// [`EditError::PathOutOfRange`]: `move_block_text` turns an anchor miss into
/// [`EditError::MoveTargetNotFound`] only for the `id::` spelling, and a child index path has one
/// miss for both ends. So the error does not say which end failed. A caller tells them apart by the
/// operation it sent and the batch error's `index`, and settles it by re-reading the page; that is
/// accepted here rather than given a new variant, because ADR-0009 keeps the addressing surface two
/// kinds wide and this refusal is already one kind of miss.
fn apply_operation(
    document: &PageDocument,
    operation: &PageOperation,
) -> Result<String, EditError> {
    match operation {
        PageOperation::EditText { block, text } => {
            document.set_block_text_text(BlockLocator::Indices(block), text)
        }
        PageOperation::SplitBlock { block, offset } => {
            document.split_block_text(BlockLocator::Indices(block), *offset)
        }
        PageOperation::MergeBlock { block } => {
            document.merge_block_text(BlockLocator::Indices(block))
        }
        PageOperation::UnwrapBlock { block } => {
            document.unwrap_block_text(BlockLocator::Indices(block))
        }
        PageOperation::IndentBlock { block } => {
            document.indent_block_text(BlockLocator::Indices(block))
        }
        PageOperation::OutdentBlock { block } => {
            document.outdent_block_text(BlockLocator::Indices(block))
        }
        PageOperation::InsertBlock { after: None, text } => document.append_block(text),
        PageOperation::InsertBlock {
            after: Some(anchor),
            text,
        } => {
            let (candidate, _new_path) = document
                .insert_block_text(MoveTarget::After(BlockLocator::Indices(anchor)), text)?;
            Ok(candidate)
        }
        PageOperation::MoveBlock { block, target } => {
            document.move_block_text(BlockLocator::Indices(block), move_target(target))
        }
        // Folding is a property, and its key and its two values are the ones `Graph::set_block_collapsed`
        // writes: `false` writes the line rather than removing it, and a batch must not carry a second
        // spelling of one state.
        PageOperation::SetCollapsed { block, collapsed } => document.upsert_block_property_text(
            BlockLocator::Indices(block),
            COLLAPSED_PROPERTY,
            collapsed_value(*collapsed),
        ),
        PageOperation::SetMarker { block, marker } => {
            document.set_block_marker_text(BlockLocator::Indices(block), *marker)
        }
    }
}

/// The document layer's version of a batch move target.
///
/// Both sides state the same four positions; only the address spelling differs, so this is a
/// translation and never a rule.
fn move_target(target: &BatchMoveTarget) -> MoveTarget<'_> {
    match target {
        BatchMoveTarget::Before { anchor } => MoveTarget::Before(BlockLocator::Indices(anchor)),
        BatchMoveTarget::After { anchor } => MoveTarget::After(BlockLocator::Indices(anchor)),
        BatchMoveTarget::FirstChildOf { anchor } => {
            MoveTarget::FirstChildOf(BlockLocator::Indices(anchor))
        }
        BatchMoveTarget::LastChildOf { anchor } => {
            MoveTarget::LastChildOf(BlockLocator::Indices(anchor))
        }
    }
}
