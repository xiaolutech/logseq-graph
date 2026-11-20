use super::error::{answer_after_write, WriteError};
use super::target::{answer_address, view_of, AnswerAddress, WriteTarget};
#[cfg(test)]
use super::TestFault;
use crate::document::PageDocument;
use crate::domain::{BlockSelector, BlockView, GraphMetadata};
use crate::engine::{baseline_for, Graph};
use crate::index::IndexError;
use crate::source::PageFile;
use std::path::PathBuf;

/// The text of the file a write just replaced, or the error that says only the index is behind.
///
/// Every write re-observes the file it replaced and reads it again, because the index must be given
/// the bytes that are actually there rather than the candidate it assembled. Both steps happen after
/// the replacement, so both failures mean the same thing: the file holds the new text and the index
/// does not. Reading through this one function is what keeps that classification in one place —
/// letting a read failure surface as [`WriteError::Source`] would tell the caller nothing was
/// written, and a caller that believes that retries the edit.
fn reread_after_write(file: &PageFile) -> Result<String, WriteError> {
    file.read_text().map_err(|_| WriteError::ReReadAfterWrite {
        page: file.name().to_owned(),
        path: file.path().to_owned(),
    })
}

impl Graph {
    /// Steps 5 to 7 of the single-page pipeline: replace the file with `candidate` atomically,
    /// re-observe it, re-read and re-parse the bytes actually on disk, and replace the whole page
    /// in one transaction that records the baseline of the observation.
    ///
    /// Every single-page edit shares this, because a write differs from the others only in what it
    /// does before this point and what it reads back after it. The caller holds the write lock;
    /// this takes none. A failure before the file is replaced leaves the graph as it was; a failure
    /// after leaves the new bytes on disk with the old index, reported as
    /// [`WriteError::ReReadAfterWrite`] or [`WriteError::IndexAfterWrite`] and repaired by the next
    /// scan. The file is never rolled back.
    pub(super) fn commit_page_edit(
        &mut self,
        file: &PageFile,
        candidate: String,
    ) -> Result<GraphMetadata, WriteError> {
        let (committed, _text) = self.commit_page_edit_observed(file, candidate)?;
        Ok(committed)
    }

    /// Steps 5 to 7 of the single-page pipeline, also answering the text the file holds after the
    /// replacement.
    ///
    /// [`Self::commit_page_edit`] is this pipeline without the text, for the writes that answer
    /// through the index alone. [`Self::commit_page`] needs the re-read bytes themselves, because it
    /// answers a whole [`PageSnapshot`](crate::domain::PageSnapshot) of the page, and a snapshot has
    /// to describe one observation rather than a candidate the engine assembled. The text returned
    /// here is the one this pipeline parsed for the projection, so the snapshot and the rows come
    /// from the same bytes. The caller holds the write lock; this takes none, and it replaces exactly
    /// one file.
    pub(in crate::engine) fn commit_page_edit_observed(
        &mut self,
        file: &PageFile,
        candidate: String,
    ) -> Result<(GraphMetadata, String), WriteError> {
        self.source.write_text(file.path(), &candidate)?;

        #[cfg(test)]
        if self.test_fault == Some(TestFault::ReReadAfterWrite) {
            self.test_fault = None;
            self.test_fault_fired
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(WriteError::ReReadAfterWrite {
                page: file.name().to_owned(),
                path: file.path().to_owned(),
            });
        }

        // The `PageFile` the caller read from carries a stat snapshot that predates the write, and
        // the index must record the observation of the text it projects, so the file is observed
        // again rather than reusing that snapshot. Only this one file is observed: the rest of the
        // graph is not this write's to project.
        let fresh = self
            .source
            .reobserve(file)
            .map_err(|_| WriteError::ReReadAfterWrite {
                page: file.name().to_owned(),
                path: file.path().to_owned(),
            })?;
        let observed = fresh.metadata();
        let fresh_text = reread_after_write(&fresh)?;
        let fresh_document = PageDocument::parse(&fresh_text);
        let baseline = baseline_for(observed, &fresh_text);

        #[cfg(test)]
        let injected_index_failure = self.test_fault == Some(TestFault::IndexAfterWrite);
        #[cfg(test)]
        if injected_index_failure {
            self.test_fault = None;
            self.test_fault_fired
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.install_test_index_trigger()
                .map_err(|cause| WriteError::IndexAfterWrite {
                    page: fresh.name().to_owned(),
                    path: fresh.path().to_owned(),
                    cause,
                })?;
        }

        // One transaction replaces the page and records the baseline together. A failure here has
        // already changed the file, which is what `IndexAfterWrite` reports.
        if let Some(previous) = self
            .index
            .page_file(file.name())
            .map_err(|cause| answer_after_write(file.name(), file.path(), cause))?
        {
            if previous != fresh.relative_path() && self.source.page_at(&previous)?.is_none() {
                self.remove_file_rows(Some(file.name()), &previous)?;
            }
        }
        let replacement = self.index.replace_page_with_baseline(
            fresh.relative_path(),
            fresh.fallback_name(),
            fresh.journal_date(),
            &fresh_document,
            baseline,
        );
        #[cfg(test)]
        if injected_index_failure {
            if let Err(cause) = self.index.drop_test_replacement_trigger() {
                return Err(WriteError::IndexAfterWrite {
                    page: fresh.name().to_owned(),
                    path: fresh.path().to_owned(),
                    cause,
                });
            }
        }
        let committed = replacement.map_err(|cause| WriteError::IndexAfterWrite {
            page: fresh.name().to_owned(),
            path: fresh.path().to_owned(),
            cause,
        })?;
        Ok((committed, fresh_text))
    }

    /// Make the next replacement fail inside SQLite itself, for the index-after-write test.
    #[cfg(test)]
    fn install_test_index_trigger(&self) -> Result<(), IndexError> {
        self.index.install_test_replacement_trigger()
    }

    /// The selector one minted block answers with: the row the projection just committed decides
    /// between the durable and the ephemeral kind, exactly as [`Self::insert_block`]'s step 10
    /// reads it.
    ///
    /// The answer comes from the projection, not from the text the call was asked with: the row
    /// the replacement just committed is what the next read of the new block resolves against,
    /// and re-deriving identity here would be a second opinion that could disagree with what was
    /// stored. The row is there because the replacement committed this page; a missing one means
    /// the stored projection contradicts itself.
    pub(super) fn answer_selector(
        &self,
        page: &str,
        path: &[u32],
    ) -> Result<BlockSelector, WriteError> {
        let located = self
            .index
            .block_by_page_path(page, path)
            .map_err(|cause| answer_after_write(page, &self.page_path(page), cause))?
            .ok_or_else(|| WriteError::IndexAfterWrite {
                page: page.to_owned(),
                path: self.page_path(page),
                cause: IndexError::CorruptRow {
                    column: "blocks.preorder",
                    value: answer_address(page, path),
                },
            })?;
        Ok(located.selector)
    }

    /// The file the index says holds `page`, for naming a failure that happened after its commit.
    fn page_path(&self, page: &str) -> PathBuf {
        self.index
            .page_file(page)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub(super) fn answer_view(
        &self,
        page: &str,
        address: AnswerAddress,
    ) -> Result<BlockView, WriteError> {
        let path = self.page_path(page);
        match address {
            AnswerAddress::Durable(id) => {
                let located = self
                    .index
                    .block_by_durable_id(&id)
                    .map_err(|cause| answer_after_write(page, &path, cause))?
                    .into_one()
                    .ok_or_else(|| WriteError::IndexAfterWrite {
                        page: page.to_owned(),
                        path: path.clone(),
                        cause: IndexError::CorruptRow {
                            column: "blocks.durable_id",
                            value: id.as_str().to_owned(),
                        },
                    })?;
                Ok(BlockView::new(
                    located.page,
                    located.preorder,
                    located.depth,
                    located.parent_preorder,
                    located.text,
                    located.marker,
                    BlockSelector::Durable(located.durable_id),
                ))
            }
            AnswerAddress::Preorder(preorder) => {
                let located = self
                    .index
                    .block_by_page_preorder(page, preorder)
                    .map_err(|cause| answer_after_write(page, &path, cause))?
                    .ok_or_else(|| WriteError::IndexAfterWrite {
                        page: page.to_owned(),
                        path: path.clone(),
                        cause: IndexError::CorruptRow {
                            column: "blocks.preorder",
                            value: format!("{page}:{preorder}"),
                        },
                    })?;
                Ok(view_of(located))
            }
            AnswerAddress::Path(address) => {
                let located = self
                    .index
                    .block_by_page_path(page, &address)
                    .map_err(|cause| answer_after_write(page, &path, cause))?
                    .ok_or_else(|| WriteError::IndexAfterWrite {
                        page: page.to_owned(),
                        path,
                        cause: IndexError::CorruptRow {
                            column: "blocks.preorder",
                            value: answer_address(page, &address),
                        },
                    })?;
                Ok(view_of(located))
            }
        }
    }

    /// The view of the block the write target names, read after the page was replaced.
    pub(super) fn answer_block(
        &self,
        page: &str,
        target: &WriteTarget,
    ) -> Result<BlockView, WriteError> {
        self.answer_view(page, target.answer_address())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Fixture;
    use super::super::TestFault;
    use super::*;
    use crate::engine::write::WriteState;

    #[test]
    fn a_re_read_failure_keeps_the_new_bytes_and_the_old_projection() {
        let fixture = Fixture::new("reread");
        fixture.write("pages/alpha.md", "- one\n");
        let mut graph = fixture.open(Some(TestFault::ReReadAfterWrite));
        let before = graph
            .page_blocks("alpha")
            .expect("read before")
            .into_value();
        let revision = graph.metadata().expect("metadata").into_value().revision();

        let error = graph
            .append_block("alpha", "two")
            .expect_err("the armed fault must fire");

        assert!(
            matches!(error, WriteError::ReReadAfterWrite { .. }),
            "{error:?}"
        );
        assert_eq!(error.write_state(), WriteState::FileChanged);
        assert_eq!(fixture.read("pages/alpha.md"), "- one\n- two\n");
        assert!(graph
            .test_fault_fired
            .load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            graph.metadata().expect("metadata").into_value().revision(),
            revision,
            "the failed commit must not advance the revision"
        );
        assert_eq!(
            graph.page_blocks("alpha").expect("read after").into_value(),
            before,
            "the old projection must stay intact"
        );

        // The next ordinary read repairs the page from the bytes the failed write left behind.
        let repaired = graph
            .reads(crate::engine::Freshness::CheckNow)
            .page_blocks("alpha")
            .expect("read with repair")
            .into_value()
            .expect("alpha exists");
        assert_eq!(
            repaired
                .iter()
                .map(|block| block.text().lines().next().unwrap())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[test]
    fn an_index_failure_after_the_write_keeps_the_file_and_old_rows() {
        let fixture = Fixture::new("index-after");
        fixture.write("pages/alpha.md", "- one\n");
        let mut graph = fixture.open(Some(TestFault::IndexAfterWrite));
        let before = graph
            .page_blocks("alpha")
            .expect("read before")
            .into_value();
        let revision = graph.metadata().expect("metadata").into_value().revision();

        let error = graph
            .append_block("alpha", "two")
            .expect_err("the SQLite trigger must reject the replacement");

        assert!(
            matches!(error, WriteError::IndexAfterWrite { .. }),
            "{error:?}"
        );
        assert_eq!(error.write_state(), WriteState::FileChanged);
        assert_eq!(fixture.read("pages/alpha.md"), "- one\n- two\n");
        assert!(graph
            .test_fault_fired
            .load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            graph.metadata().expect("metadata").into_value().revision(),
            revision,
            "the rolled-back transaction must not advance the revision"
        );
        assert_eq!(
            graph.page_blocks("alpha").expect("read after").into_value(),
            before,
            "the old projection must stay complete"
        );

        let repaired = graph
            .reads(crate::engine::Freshness::CheckNow)
            .page_blocks("alpha")
            .expect("read with repair")
            .into_value()
            .expect("alpha exists");
        assert_eq!(
            repaired
                .iter()
                .map(|block| block.text().lines().next().unwrap())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[test]
    fn an_answer_read_failure_after_the_commit_is_file_changed() {
        let fixture = Fixture::new("answer-after");
        fixture.write("pages/alpha.md", "- one\n");
        let mut graph = fixture.open(None);
        let revision = graph.metadata().expect("metadata").into_value().revision();
        graph.index.fail_next_page_metadata();

        let error = graph
            .append_block("alpha", "two")
            .expect_err("the answer read is armed to fail");

        assert!(
            matches!(error, WriteError::IndexAfterWrite { .. }),
            "expected the post-commit answer error, got {error:?}"
        );
        assert_eq!(error.write_state(), WriteState::FileChanged);
        assert_eq!(
            fixture.read("pages/alpha.md"),
            "- one\n- two\n",
            "the file replacement happened before the answer read"
        );
        assert_eq!(
            graph.metadata().expect("metadata").into_value().revision(),
            revision + 1,
            "the projection transaction committed before the answer read"
        );
        assert_eq!(
            graph
                .page_blocks("alpha")
                .expect("read the committed rows")
                .into_value()
                .expect("alpha exists")
                .iter()
                .map(|block| block.text().lines().next().unwrap())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }
}
