use super::error::{answer_after_write, WriteError};
use super::target::{check_ephemeral_freshness, ResolvedTarget, WriteTarget};
#[cfg(test)]
use super::TestFault;
use crate::document::{BlockLocator, MoveTarget as DocumentMoveTarget, PageDocument};
use crate::domain::{BlockSelector, GraphMetadata, MoveBlockAddress, PageMetadata, Read};
use crate::engine::{baseline_for, EngineError, Graph};
use crate::index::{IndexError, MoveAnchorKind, MoveIntent};
use crate::source::{self, PageFile, SourceError};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

/// Where a moved block should end up, named by a selector for an existing block.
///
/// This is [`Graph::move_block`]'s parameter, and it differs from the document layer's
/// [`document::MoveTarget`](crate::document::MoveTarget) in one thing only: the addresses are
/// [`BlockSelector`]s, so a caller can name the anchor block by an ephemeral id as well as by its
/// `id::`. The selector is resolved against this graph and its index before the edit runs — the same
/// resolution a moved block gets — and the resulting locator is what the edit is applied with. The
/// conversion happens where the edit is applied, so neither layer re-states the other's rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveTarget {
    /// Immediately before the block this selector names.
    Before(BlockSelector),
    /// Immediately after the block this selector names, as its next sibling.
    After(BlockSelector),
    /// As the first child of the block this selector names.
    FirstChildOf(BlockSelector),
    /// As the last child of the block this selector names, after every existing child.
    LastChildOf(BlockSelector),
}

impl MoveTarget {
    /// The selector of the block this target is stated in terms of.
    pub(super) fn anchor(&self) -> &BlockSelector {
        match self {
            Self::Before(selector)
            | Self::After(selector)
            | Self::FirstChildOf(selector)
            | Self::LastChildOf(selector) => selector,
        }
    }

    /// The same target, spelled with the locator the document layer reads.
    ///
    /// The caller has already resolved [`anchor`](Self::anchor) — a durable id stays the id the
    /// document resolves against the file, and an ephemeral id became the child-index path its row
    /// names — so this only chooses which of the four positions the edit should use.
    pub(super) fn as_document_target<'a>(
        &self,
        anchor: BlockLocator<'a>,
    ) -> DocumentMoveTarget<'a> {
        match self {
            Self::Before(_) => DocumentMoveTarget::Before(anchor),
            Self::After(_) => DocumentMoveTarget::After(anchor),
            Self::FirstChildOf(_) => DocumentMoveTarget::FirstChildOf(anchor),
            Self::LastChildOf(_) => DocumentMoveTarget::LastChildOf(anchor),
        }
    }
}

impl Graph {
    /// Move the block `selector` names, and every block under it, next to the block named by
    /// `target`, within the same page.
    ///
    /// The block keeps its content, its marker bytes, and its identity lines: a move is a change of
    /// position, not of text. Indentation is what turns the position into structure — the moved
    /// subtree is re-indented to sit where the target says, keeping its internal shape — and the
    /// document layer owns that rule ([`PageDocument::move_block_text`]).
    ///
    /// # Identity of the two ends
    ///
    /// The moved block can be named by a durable id or by an ephemeral id, exactly as
    /// [`Self::update_block`] and [`Self::remove_block`] accept them: an ephemeral move edits by the
    /// child-index path its row names, against the file, after the row's fingerprint has been
    /// checked against the bytes (ADR-0012). A same-page move is therefore fine with either kind at
    /// either end, and both ends share one fingerprint check because both positions were computed
    /// from the same projected bytes.
    ///
    /// Both identities are accepted at either end of a cross-page move too, without adding `id::`.
    ///
    /// # One page, or two
    ///
    /// A move whose two ends resolve to one page goes through this path, which commits exactly one
    /// page in one transaction (ADR-0004). When they resolve to two pages, `Self::move_across_pages`
    /// runs instead: the same lock, lookups, and refusal rules, followed by a two-file write under a
    /// [`MoveIntent`] and one multi-page commit. The intent records the moved block and its anchor by
    /// durable id or a child-index path resolved against the corresponding pre bytes after the
    /// fingerprint gate. Recovery resolves a source path only when those bytes still match the
    /// source pre hash, then verifies the derived removal against the source post hash. No
    /// ephemeral UUID is persisted and no durable identity is granted as a side effect.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Graph::remove_block`]'s, once the two ends are resolved through the index (one
    /// query per end, one `ScanMode::ReadEveryFile` fallback per durable miss and none for an
    /// ephemeral one): locate the page's file, read it, check the fingerprints the rows recorded,
    /// apply the pure edit, replace the file atomically, re-observe and re-read it, replace the whole
    /// page in one transaction with the baseline of that observation, and answer the metadata the
    /// replacement just committed. Every failure before step 5 leaves both files exactly as they
    /// were; a failure in step 6 or 7 leaves the new file with the old index, reported as
    /// [`WriteError::ReReadAfterWrite`] or [`WriteError::IndexAfterWrite`] and repaired by the next
    /// scan.
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives, and it
    /// reprojects only the page it rewrote.
    pub fn move_block(
        &mut self,
        selector: &BlockSelector,
        target: MoveTarget,
    ) -> Result<Read<PageMetadata>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_selector_write(&[selector, target.anchor()])?;

        // Step 2: resolve both ends to their pages. The anchor is resolved the same way the moved
        // block is, so a durable anchor that only an unprojected file states pays for one scan
        // rather than being reported as missing, and an ephemeral one resolves through the index to
        // the child-index path its row names plus the fingerprint of the bytes that row describes.
        let source = self.write_target(selector)?;
        let anchor = self.resolve_anchor(target.anchor())?;

        // Two pages mean two file replacements, which the recovery protocol of
        // [`Self::move_across_pages`] exists for (ADR-0004). Every refusal so far has left both
        // files as they were; candidates and their pre-hash-bound addresses are derived under this
        // same lock before the intent is persisted.
        if source.page() != anchor.page {
            return self.move_across_pages(source, target, anchor);
        }

        // Step 3: resolve the page name the index stored to the file that holds it.
        let file = self.locate_page(source.page())?;

        // Step 4: the edit is applied to the bytes on disk and stays a pure transformation
        // until the file is replaced. Both ends live on this one page, so one fingerprint check
        // covers both coordinates: they were computed from the same projected bytes.
        let original = file.read_text()?;
        check_ephemeral_freshness(file.name(), &original, source.page_fingerprint())?;
        check_ephemeral_freshness(file.name(), &original, anchor.page_fingerprint)?;
        let candidate = PageDocument::parse(&original).move_block_text(
            source.locator(),
            target.as_document_target(anchor.locator.as_locator()),
        )?;

        // Steps 5 to 7 are the shared single-page commit. The caller still holds the write lock.
        let committed = self.commit_page_edit(&file, candidate)?;

        // The metadata comes from the projection, like the append's and the remove's: a move keeps
        // every block, so `block_count` is the same number as before, and it is read from what was
        // stored rather than from the candidate this call parsed.
        let metadata = self
            .index
            .page_metadata(file.name())
            .map_err(|cause| answer_after_write(file.name(), file.path(), cause))?
            .ok_or_else(|| WriteError::IndexAfterWrite {
                page: file.name().to_owned(),
                path: file.path().to_owned(),
                cause: IndexError::CorruptRow {
                    column: "pages.name",
                    value: file.name().to_owned(),
                },
            })?;

        Ok(Read::new(
            metadata,
            self.read_meta(committed, self.is_stale()),
        ))
    }

    /// The two-file half of [`Self::move_block`]: the ids resolved to two pages, so the write
    /// replaces both files under a [`MoveIntent`] and projects both pages in one commit.
    ///
    /// # Order and failure
    ///
    /// The steps are the single-file path's until the write, and then they differ in exactly the
    /// ways ADR-0004's multi-file clause requires:
    ///
    /// 1. **Take the write lock** and resolve both ids, exactly as the same-page path does; every
    ///    ambiguity and not-found answer above is the same one rule.
    /// 2. **Read both files and derive both candidates** with
    ///    [`PageDocument::move_block_text_to_page`], a pure edit over the bytes on disk. The
    ///    fingerprints of all four states — both files before and both candidates — are computed
    ///    here, from the same fingerprint the baselines store.
    /// 3. **Commit the [`MoveIntent`] before the first file write.** After this point the write is
    ///    recoverable rather than refusable: the intent says what the write was trying to reach,
    ///    which file state alone cannot (ADR-0004).
    /// 4. **Write the destination, then the source.** Destination first is what makes the crash
    ///    states enumerable: the destination never shows a post hash while the source is untouched
    ///    by this write's own order, so recovery can name that combination as outside interference.
    /// 5. **Re-observe, re-read, and re-parse both files**, and commit both fresh parses in one
    ///    multi-page transaction, which clears the intent and advances the revision exactly once.
    ///
    /// A failure before step 3 leaves both files byte-identical and no intent behind. A destination
    /// write failure is inspected against the pre hash: unchanged destination means the source was
    /// never touched, so the intent is cleared and the ordinary unwritten error is returned;
    /// otherwise, and always after a source write failure, the intent stays and the error is
    /// [`WriteError::PartialMove`]. A failure in step 5 keeps the intent too, and reports the
    /// file-changed categories the single-file paths already use — [`WriteError::ReReadAfterWrite`]
    /// or [`WriteError::IndexAfterWrite`] — because both files hold the post state and only the
    /// index is behind. No file write is ever rolled back, and no original bytes are ever written
    /// back as recovery.
    fn move_across_pages(
        &mut self,
        source: WriteTarget,
        target: MoveTarget,
        anchor: ResolvedTarget,
    ) -> Result<Read<PageMetadata>, WriteError> {
        // Step 2: both files, both candidates, all four fingerprints. Nothing has changed yet: a
        // refusal from the edit — a missing or ambiguous id the lookup could not see — leaves both
        // files byte-identical and no intent behind. An ephemeral anchor is checked against the
        // destination bytes before they are edited, for the same reason a same-page move checks its
        // two ends: the anchor's child path is a coordinate in the bytes its row describes, and
        // placing the block beside whatever now sits at that path would be the aliasing this
        // identity exists to prevent (ADR-0012).
        let source_file = self.locate_page(source.page())?;
        let destination_file = self.locate_page(&anchor.page)?;
        let source_text = source_file.read_text()?;
        let destination_text = destination_file.read_text()?;
        check_ephemeral_freshness(source_file.name(), &source_text, source.page_fingerprint())?;
        check_ephemeral_freshness(
            destination_file.name(),
            &destination_text,
            anchor.page_fingerprint,
        )?;
        let source_document = PageDocument::parse(&source_text);
        let destination_document = PageDocument::parse(&destination_text);
        let moved = match &source {
            WriteTarget::Durable { id, .. } => MoveBlockAddress::Durable(id.clone()),
            WriteTarget::Ephemeral(_) => {
                MoveBlockAddress::Path(source_document.block_child_path(source.locator())?)
            }
        };
        let anchor_address = match target.anchor() {
            BlockSelector::Durable(id) => MoveBlockAddress::Durable(id.clone()),
            BlockSelector::Ephemeral(_) => MoveBlockAddress::Path(
                destination_document.block_child_path(anchor.locator.as_locator())?,
            ),
        };
        let (source_candidate, destination_candidate) = source_document.move_block_text_to_page(
            source.locator(),
            &destination_document,
            target.as_document_target(anchor.locator.as_locator()),
        )?;
        let source_pre_hash = source::content_fingerprint(&source_text);
        let source_post_hash = source::content_fingerprint(&source_candidate);
        let destination_pre_hash = source::content_fingerprint(&destination_text);
        let destination_post_hash = source::content_fingerprint(&destination_candidate);

        // Step 3: the intent goes in before the first byte changes. One writer holds the lock, so
        // overwriting any stale row is sound; from here the next open reconciles the move instead
        // of this call having to refuse or guess.
        self.journal.write(&MoveIntent {
            moved,
            anchor: anchor_address,
            anchor_kind: match target {
                MoveTarget::Before(_) => MoveAnchorKind::Before,
                MoveTarget::After(_) => MoveAnchorKind::After,
                MoveTarget::FirstChildOf(_) => MoveAnchorKind::FirstChildOf,
                MoveTarget::LastChildOf(_) => MoveAnchorKind::LastChildOf,
            },
            from_relative_path: source_file.relative_path().to_owned(),
            from_page: source_file.name().to_owned(),
            from_pre_hash: source_pre_hash,
            from_post_hash: source_post_hash,
            to_relative_path: destination_file.relative_path().to_owned(),
            to_page: destination_file.name().to_owned(),
            to_pre_hash: destination_pre_hash,
            to_post_hash: destination_post_hash,
        })?;

        // The error of a write that stopped between the intent and the final commit. The intent is
        // kept in every case but one: a destination write that left the destination at its pre hash
        // proves the source was never touched, so the move never started and the ordinary unwritten
        // answer is honest.
        let partial = |cause: String| WriteError::PartialMove {
            from: source_file.name().to_owned(),
            from_path: source_file.path().to_owned(),
            to: destination_file.name().to_owned(),
            to_path: destination_file.path().to_owned(),
            cause,
        };

        // Step 4: destination first. Source second, because a source written before the
        // destination would make "destination pre, source post" a state this write's own order
        // can produce, and recovery could then no longer read that combination as interference.
        // No failure here rolls a write back: the bytes that landed stay (ADR-0004).
        if let Err(error) = self
            .source
            .write_text(destination_file.path(), &destination_candidate)
        {
            // The rename either replaced the bytes or did not. Reading what is actually there
            // decides: unchanged means the move never started and is refused as an ordinary
            // failure, with the intent cleared because this call is its only possible owner.
            let changed = self
                .source
                .page_at(destination_file.relative_path())
                .and_then(|file| {
                    file.ok_or(SourceError::Io {
                        path: destination_file.path().to_owned(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "the destination file is gone",
                        ),
                    })
                })
                .and_then(|file| file.read_text())
                .map(|current| source::content_fingerprint(&current) != destination_pre_hash);
            match changed {
                Ok(false) => {
                    self.journal.clear()?;
                    return Err(WriteError::Source(error));
                }
                // Changed, or unreadable: either way the destination may hold the new bytes, so
                // the intent stays and the next open reconciles the move from it.
                _ => return Err(partial(error.to_string())),
            }
        }

        // The destination is written. A source failure after this is always partial: the source
        // file may be pre or post — a rename can land before a later fsync step fails, and the
        // bytes on disk are the authority — so neither file is inspected and the intent says what
        // completion needs.
        #[cfg(test)]
        let fail_source = self.test_fault == Some(TestFault::SourceAfterDestination);
        #[cfg(test)]
        if fail_source {
            self.test_fault = None;
            self.test_fault_fired
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        #[cfg(test)]
        let source_write = if fail_source {
            Err(SourceError::Committed {
                path: source_file.path().to_owned(),
                step: source_file
                    .path()
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_owned(),
                source: std::io::Error::other("injected source write failure"),
            })
        } else {
            self.source
                .write_text(source_file.path(), &source_candidate)
        };
        #[cfg(not(test))]
        let source_write = self
            .source
            .write_text(source_file.path(), &source_candidate);
        if let Err(error) = source_write {
            return Err(partial(error.to_string()));
        }

        // Step 5: project what is actually on disk, not the candidates. The multi-page commit
        // replaces both pages, clears the intent, and advances the revision once. A failure here
        // has already changed both files, so it is reported as the file-changed category the
        // single-file paths use; the intent stays and the next open finishes the commit.
        let committed = self
            .commit_moved_pages(&source_file, &destination_file)
            .map_err(MoveCommitError::into_write_error)?;

        let metadata = self
            .index
            .page_metadata(destination_file.name())
            .map_err(|cause| {
                answer_after_write(destination_file.name(), destination_file.path(), cause)
            })?
            .ok_or_else(|| WriteError::IndexAfterWrite {
                page: destination_file.name().to_owned(),
                path: destination_file.path().to_owned(),
                cause: IndexError::CorruptRow {
                    column: "pages.name",
                    value: destination_file.name().to_owned(),
                },
            })?;

        Ok(Read::new(
            metadata,
            self.read_meta(committed, self.is_stale()),
        ))
    }

    /// Re-observe, re-read, and re-parse the two moved files, and commit both pages in one
    /// transaction, answering the committed revision.
    ///
    /// The parse the candidates came from is what the write meant to produce; the fresh reads are
    /// what is actually there, and they are also what the baselines have to describe, so every
    /// input of this commit is taken from the files, never from a candidate.
    ///
    /// A failure keeps the intent in place: the next open either completes the move from it or
    /// reports the file that can no longer be observed, so the intent is never cleared by a call
    /// that did not make it spent.
    pub(super) fn commit_moved_pages(
        &mut self,
        source_file: &PageFile,
        destination_file: &PageFile,
    ) -> Result<GraphMetadata, MoveCommitError> {
        // Re-observation can fail when another process removed or renamed a file inside the window.
        let fresh_source =
            self.source
                .reobserve(source_file)
                .map_err(|_| MoveCommitError::ReRead {
                    page: source_file.name().to_owned(),
                    path: source_file.path().to_owned(),
                })?;
        let fresh_destination =
            self.source
                .reobserve(destination_file)
                .map_err(|_| MoveCommitError::ReRead {
                    page: destination_file.name().to_owned(),
                    path: destination_file.path().to_owned(),
                })?;

        let mut pages = Vec::with_capacity(2);
        for file in [fresh_source, fresh_destination] {
            let observed = file.metadata();
            let text = file.read_text().map_err(MoveCommitError::Source)?;
            let document = PageDocument::parse(&text);
            let baseline = baseline_for(observed, &text);
            pages.push((
                file.relative_path().to_owned(),
                file.fallback_name().to_owned(),
                file.journal_date(),
                document,
                baseline,
            ));
        }

        // One transaction covers both pages, so a failure names the page written first: the
        // destination is the file this move's own order committed first, and the intent says the
        // rest.
        let metadata = self
            .index
            .replace_pages_with_baselines(&pages)
            .map_err(|cause| MoveCommitError::Index {
                page: destination_file.name().to_owned(),
                path: destination_file.path().to_owned(),
                cause,
            })?;
        self.journal.clear().map_err(|error| {
            MoveCommitError::Source(source::after_commit(source_file.path(), error))
        })?;
        Ok(metadata)
    }

    /// Project both moved files as they actually are, after outside interference.
    ///
    /// No bytes are changed. The commit clears the intent and advances the revision once, which is
    /// what makes the index current without pretending the move completed.
    pub(super) fn commit_reconciled_pages(
        &mut self,
        source_file: &PageFile,
        destination_file: &PageFile,
    ) -> Result<(), EngineError> {
        self.commit_moved_pages(source_file, destination_file)
            .map(|_| ())
            .map_err(MoveCommitError::into_engine_error)
    }
}

/// Which of an intent's two known states one file's current bytes match, or neither.
pub(super) enum MoveFileState {
    Pre,
    Post,
    /// The bytes match no recorded hash: an outside edit the intent cannot describe.
    Other,
}

pub(super) fn move_file_state(current: u64, pre: u64, post: u64) -> MoveFileState {
    if current == pre {
        MoveFileState::Pre
    } else if current == post {
        MoveFileState::Post
    } else {
        MoveFileState::Other
    }
}

/// Why the final commit of a move could not be made.
///
/// The three failures the commit can produce, kept apart from [`WriteError`] because the same
/// commit is reached from a write and from recovery, and each caller has its own error type for
/// them. Exhaustive on purpose: the commit reads and projects files and never writes one, so no
/// other failure exists and neither caller needs a catch-all arm.
pub(super) enum MoveCommitError {
    /// A file could not be read again after the write.
    Source(SourceError),
    /// A written file can no longer be observed.
    ReRead {
        /// The page as it was named when the file was replaced.
        page: String,
        /// The file that holds the new text.
        path: PathBuf,
    },
    /// The multi-page index transaction failed.
    Index {
        /// The page written first, which the failure is reported against.
        page: String,
        /// The file that holds that page's new text.
        path: PathBuf,
        /// Why the transaction failed.
        cause: IndexError,
    },
}

impl MoveCommitError {
    /// The same failure as a write reports it.
    fn into_write_error(self) -> WriteError {
        match self {
            Self::Source(error) => WriteError::Source(error),
            Self::ReRead { page, path } => WriteError::ReReadAfterWrite { page, path },
            Self::Index { page, path, cause } => WriteError::IndexAfterWrite { page, path, cause },
        }
    }

    /// The same failure as an open reports it, which is the file or index category the open already
    /// has; the intent stays either way, so the next open retries the commit.
    pub(super) fn into_engine_error(self) -> EngineError {
        match self {
            Self::Source(error) => EngineError::Source(error),
            Self::ReRead { page, path } => EngineError::Source(SourceError::Io {
                path,
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "the file {page:?} of a pending move can no longer be observed for \
                         recovery"
                    ),
                ),
            }),
            Self::Index { cause, .. } => EngineError::Index(cause),
        }
    }
}
