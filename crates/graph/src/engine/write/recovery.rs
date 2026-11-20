use super::moves::{move_file_state, MoveCommitError, MoveFileState};
use crate::document::{BlockLocator, PageDocument};
use crate::domain::MoveBlockAddress;
use crate::engine::{EngineError, Graph, ScanMode};
use crate::source::{self, PageFile, SourceError, WriteLock};
use std::path::Path;

impl Graph {
    /// Finish or reconcile a cross-page move that a previous process left behind, on every open
    /// and before any startup scan.
    ///
    /// A surviving [`MoveIntent`] is the one state a write may leave behind that a scan cannot
    /// interpret: the scan knows that a file differs from its baseline, but not that two files
    /// belong to one interrupted write. Only the intent knows that, so recovery runs while the
    /// intent is still there, including when the startup scan is disabled — a write-only caller is
    /// exactly the one whose next write must not land on a mid-move graph.
    ///
    /// # Order and failure
    ///
    /// The journal is read under the graph lock, independent of the index. Both files are read and hashed, and the
    /// state they hold decides:
    ///
    /// - **both pre** — the crash landed between the intent and the destination write. Nothing was
    ///   written, so the intent is cleared, nothing else changes, and the revision does not move.
    /// - **destination post, source pre** — the destination landed and the crash stopped the
    ///   source. The source removal is re-derived from the current source file, verified against
    ///   the recorded post fingerprint, written, and both fresh parses are committed in one
    ///   multi-page transaction.
    /// - **both post** — the crash landed between the source write and the index commit. The
    ///   bytes are already the intended result; the same multi-page commit finishes it.
    /// - **destination pre with source post**, and **any hash that is neither pre nor post** —
    ///   outside interference. Recovery does not guess: both files are projected as they actually
    ///   are, which also clears the intent and advances the revision once, so the index is current
    ///   whatever the bytes say.
    ///
    /// A missing or unreadable file keeps the intent and fails the open: recovering from a graph
    /// this broken is a person's decision, not a heuristic. A write or index failure during a
    /// completing recovery also keeps the intent and fails the open — the intent is cleared only
    /// by the commit that makes it spent, or by the both-pre answer that proves it never meant
    /// anything.
    pub(crate) fn recover_move_intent(&mut self) -> Result<(), EngineError> {
        let (_lock, intent) = loop {
            let lock = match WriteLock::acquire_for_scan(
                &self.write_lock_dir,
                Some(self.write_lock_wait),
            )? {
                source::ScanLock::Held(lock) => lock,
                source::ScanLock::Unavailable(error) => {
                    return if self.journal.is_present()? {
                        Err(error.into())
                    } else {
                        Ok(())
                    };
                }
            };
            self.refresh_index()?;
            let Some(intent) = self.journal.read()? else {
                return Ok(());
            };
            if self.index.configuration_matches(self.source.signature())? {
                break (lock, intent);
            }
            drop(lock);
            self.scan(ScanMode::TrustStat)?;
        };

        // Both files, as they are now. A missing or unreadable file is not recoverable state: the
        // intent stays and the open fails, because a heuristic that "recovers" by inventing bytes
        // is the kind of guess this repository refuses.
        let read_file = |relative_path: &Path| -> Result<(PageFile, String, u64), EngineError> {
            let file = self.source.page_at(relative_path)?.ok_or_else(|| {
                EngineError::Source(SourceError::Io {
                    path: self.source.root().join(relative_path),
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "the file a pending move intent names is gone",
                    ),
                })
            })?;
            let text = file.read_text()?;
            let hash = source::content_fingerprint(&text);
            Ok((file, text, hash))
        };
        let (source_file, source_text, source_hash) = read_file(&intent.from_relative_path)?;
        let (destination_file, _destination_text, destination_hash) =
            read_file(&intent.to_relative_path)?;

        let source_state =
            move_file_state(source_hash, intent.from_pre_hash, intent.from_post_hash);
        let destination_state =
            move_file_state(destination_hash, intent.to_pre_hash, intent.to_post_hash);
        match (source_state, destination_state) {
            // The move never started: nothing was written, so the intent is spent without any
            // write and without a revision advance.
            (MoveFileState::Pre, MoveFileState::Pre) => {
                self.journal.clear()?;
                Ok(())
            }
            // The destination landed; finish the source and commit both pages as they are.
            (MoveFileState::Pre, MoveFileState::Post) => {
                // Re-derive the removal from the current source, not from stored bytes, and
                // verify it against the recorded post hash before writing anything. A mismatch
                // is interference in the one file the recovery is about to rewrite, so it falls
                // to the reconcile arm below instead of being overwritten.
                let derived = PageDocument::parse(&source_text)
                    .remove_block_text(match &intent.moved {
                        MoveBlockAddress::Durable(id) => BlockLocator::Id(id.as_str()),
                        MoveBlockAddress::Path(path) => BlockLocator::Indices(path),
                    })
                    .map_err(|error| {
                        EngineError::Source(SourceError::Io {
                            path: source_file.path().to_owned(),
                            source: std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!(
                                    "the source file of a pending move no longer parses for \
                                     recovery: {error}"
                                ),
                            ),
                        })
                    })?;
                if source::content_fingerprint(&derived) != intent.from_post_hash {
                    self.commit_reconciled_pages(&source_file, &destination_file)
                } else {
                    self.source.write_text(source_file.path(), &derived)?;
                    self.commit_moved_pages(&source_file, &destination_file)
                        .map(|_| ())
                        .map_err(MoveCommitError::into_engine_error)
                }
            }
            // The bytes are already the intended result; only the index commit is missing.
            (MoveFileState::Post, MoveFileState::Post) => self
                .commit_moved_pages(&source_file, &destination_file)
                .map(|_| ())
                .map_err(MoveCommitError::into_engine_error),
            // Destination pre with source post, or a hash that matches neither: outside
            // interference. Preserve the bytes and project both files as they actually are, which
            // clears the intent and advances the revision once.
            _ => self.commit_reconciled_pages(&source_file, &destination_file),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Fixture;
    use super::super::TestFault;
    use super::*;
    use crate::domain::{BlockId, BlockSelector};
    use crate::engine::write::WriteError;
    use crate::engine::write::{MoveTarget, WriteState};
    use std::fs;

    #[test]
    fn a_partial_positional_move_recovers_using_child_indices_in_the_pre_bytes() {
        let fixture = Fixture::new("partial-positional-move");
        fixture.write(
            "pages/alpha.md",
            "- prior\n  - nested\n- moved\n  - child\n",
        );
        fixture.write("pages/beta.md", "- before\n  - nested\n- anchor\n");
        let mut graph = fixture.open(Some(TestFault::SourceAfterDestination));
        let moved = graph
            .page_blocks("alpha")
            .expect("blocks")
            .into_value()
            .expect("page")[2]
            .selector()
            .clone();
        let anchor = graph
            .page_blocks("beta")
            .expect("blocks")
            .into_value()
            .expect("page")[2]
            .selector()
            .clone();
        let error = graph
            .move_block(&moved, MoveTarget::After(anchor))
            .expect_err("source fault");
        assert!(matches!(error, WriteError::PartialMove { .. }));
        let intent = graph.journal.read().expect("intent").expect("pending move");
        assert_eq!(intent.moved, MoveBlockAddress::Path(vec![1]));
        assert_eq!(intent.anchor, MoveBlockAddress::Path(vec![1]));
        assert_eq!(
            fixture.read("pages/alpha.md"),
            "- prior\n  - nested\n- moved\n  - child\n"
        );
        drop(graph);
        let reopened = Graph::open(
            crate::engine::OpenOptions::new(&fixture.graph_root, &fixture.index_path)
                .with_startup_scan(false),
        )
        .expect("recover positional removal");
        assert_eq!(fixture.read("pages/alpha.md"), "- prior\n  - nested\n");
        assert_eq!(
            fixture.read("pages/beta.md"),
            "- before\n  - nested\n- anchor\n- moved\n  - child\n"
        );
        assert!(reopened.journal.read().expect("spent intent").is_none());
    }

    #[test]
    fn a_partial_cross_page_move_keeps_the_intent_and_recovery_finishes_it() {
        let fixture = Fixture::new("partial-move");
        fixture.write(
            "pages/alpha.md",
            "- one\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  - child\n- two\n",
        );
        fixture.write(
            "pages/beta.md",
            "- bravo\n  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n",
        );
        let mut graph = fixture.open(Some(TestFault::SourceAfterDestination));
        let revision = graph.metadata().expect("metadata").into_value().revision();

        let error = graph
            .move_block(
                &BlockSelector::Durable(
                    BlockId::parse("01234567-89ab-cdef-0123-456789abcdef").expect("valid id"),
                ),
                MoveTarget::After(BlockSelector::Durable(
                    BlockId::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("valid id"),
                )),
            )
            .expect_err("the source replacement is injected to fail");

        assert!(matches!(error, WriteError::PartialMove { .. }), "{error:?}");
        assert_eq!(error.write_state(), WriteState::FileChanged);
        assert!(graph
            .test_fault_fired
            .load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            fixture.read("pages/alpha.md"),
            "- one\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  - child\n- two\n",
            "the source keeps its pre bytes"
        );
        assert_eq!(
            fixture.read("pages/beta.md"),
            "- bravo\n  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n- one\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  - child\n",
            "the destination keeps the bytes this write committed first"
        );
        assert!(
            graph.journal.read().expect("read intent").is_some(),
            "a partial move keeps the intent for recovery"
        );
        assert_eq!(
            graph.metadata().expect("metadata").into_value().revision(),
            revision,
            "nothing was projected"
        );
        drop(graph);

        let mut reopened = Graph::open(
            crate::engine::OpenOptions::new(&fixture.graph_root, &fixture.index_path)
                .with_startup_scan(false),
        )
        .expect("reopen recovers the move");
        assert_eq!(
            reopened
                .metadata()
                .expect("metadata")
                .into_value()
                .revision(),
            revision + 1,
            "recovery commits both pages once"
        );
        assert!(
            reopened.journal.read().expect("read intent").is_none(),
            "recovery clears the spent intent"
        );
        assert_eq!(
            fixture.read("pages/alpha.md"),
            "- two\n",
            "recovery finishes the source removal"
        );
        assert_eq!(
            fixture.read("pages/beta.md"),
            "- bravo\n  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n- one\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  - child\n"
        );
    }

    #[test]
    fn partial_moves_recover_without_their_original_index() {
        for scenario in ["different", "deleted", "discarded"] {
            for fault in [None, Some(TestFault::SourceAfterDestination)] {
                let fixture = Fixture::new(scenario);
                fixture.write(
                    "pages/alpha.md",
                    "- one\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n- two\n",
                );
                fixture.write(
                    "pages/beta.md",
                    "- bravo\n  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n",
                );
                let mut graph = fixture.open(fault);
                if fault.is_none() {
                    graph
                        .index
                        .install_test_replacement_trigger()
                        .expect("fail index commit after source write");
                }
                let error = graph
                    .move_block(
                        &BlockSelector::Durable(
                            BlockId::parse("01234567-89ab-cdef-0123-456789abcdef").expect("id"),
                        ),
                        MoveTarget::After(BlockSelector::Durable(
                            BlockId::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("id"),
                        )),
                    )
                    .expect_err("index commit rejected");
                assert_eq!(error.write_state(), WriteState::FileChanged);
                assert!(graph.journal.read().expect("pending journal").is_some());
                if fault.is_none() {
                    assert_eq!(fixture.read("pages/alpha.md"), "- two\n");
                }
                let physical = graph
                    .index
                    .storage_path()
                    .expect("physical index")
                    .to_owned();
                drop(graph);
                let index = match scenario {
                    "different" => fixture.root.join("other.db"),
                    "deleted" => {
                        fs::remove_file(&physical).expect("delete active index");
                        fs::remove_file(format!("{}.current", fixture.index_path.display()))
                            .expect("delete generation pointer");
                        fs::remove_file(&fixture.index_path).expect("delete initial index");
                        fixture.index_path.clone()
                    }
                    _ => {
                        let conn = rusqlite::Connection::open(&physical).expect("index");
                        conn.execute("UPDATE graph SET schema_version = 1", [])
                            .expect("force discard");
                        fixture.index_path.clone()
                    }
                };
                let mut reopened =
                    Graph::open(crate::engine::OpenOptions::new(&fixture.graph_root, index))
                        .expect("recover through any index");
                assert!(!fixture.graph_root.join("write-intent.json").exists());
                assert_eq!(fixture.read("pages/alpha.md"), "- two\n");
                assert_eq!(
                    reopened
                        .page_metadata("alpha")
                        .expect("alpha")
                        .present()
                        .expect("page")
                        .block_count(),
                    1
                );
                assert_eq!(
                    reopened
                        .page_metadata("beta")
                        .expect("beta")
                        .present()
                        .expect("page")
                        .block_count(),
                    2
                );
            }
        }
    }
}
