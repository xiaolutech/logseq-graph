use std::path::Path;

use rusqlite::{Transaction, TransactionBehavior};

use super::{baseline, find_page_id, schema, FileBaseline, Index, IndexError};
use crate::domain::{page_name_key, GraphMetadata};

impl Index {
    /// Make the next page replacement fail inside SQLite, for an engine test.
    ///
    /// A trigger is a real constraint failure produced by the same transaction the writes use, not a
    /// fabricated error value. It is installed on this connection only and removed after the next
    /// `page_block` insert would have been rejected, so it cannot leak into another test.
    #[cfg(test)]
    pub(crate) fn install_test_replacement_trigger(&self) -> Result<(), IndexError> {
        self.conn.execute_batch(
            "CREATE TRIGGER reject_test_replacement
             BEFORE INSERT ON blocks
             BEGIN SELECT RAISE(ABORT, 'rejected by engine test'); END;",
        )?;
        Ok(())
    }

    /// Remove the trigger installed by [`Self::install_test_replacement_trigger`].
    #[cfg(test)]
    pub(crate) fn drop_test_replacement_trigger(&self) -> Result<(), IndexError> {
        self.conn
            .execute_batch("DROP TRIGGER IF EXISTS reject_test_replacement")?;
        Ok(())
    }

    /// Arm one page-metadata answer failure, for the engine's post-commit answer tests.
    #[cfg(test)]
    pub(crate) fn fail_next_page_metadata(&self) {
        self.fail_next_answer
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arm one graph-metadata answer failure, for the engine's post-commit answer tests.
    #[cfg(test)]
    pub(crate) fn fail_next_graph_metadata(&self) {
        self.fail_next_metadata
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Record a baseline without touching page rows.
    ///
    /// Used when a file's observation moved but its content did not: re-projecting it would advance
    /// the revision without changing any derived fact.
    pub fn record_file_baseline(
        &mut self,
        relative_path: &Path,
        baseline: FileBaseline,
    ) -> Result<(), IndexError> {
        baseline::write(&self.conn, relative_path, baseline)
    }

    /// Remove the baseline of `relative_path`, which the scan does only after the page rows that
    /// pointed at it are gone.
    ///
    /// `pages.relative_path` references `file_baseline.relative_path` without `ON DELETE`, so a
    /// baseline a page still points at cannot be removed: the removal fails with a constraint
    /// violation instead of leaving a page row that no longer says where it came from. The scan's
    /// disappearance path deletes the page first, for that reason.
    ///
    /// A baseline is not a product fact, so removing it does not advance the revision; the page
    /// removal that normally accompanies it does.
    pub fn remove_file_baseline(&mut self, relative_path: &Path) -> Result<(), IndexError> {
        baseline::delete(&self.conn, relative_path)
    }

    /// Delete every derived row of `name` in one transaction.
    ///
    /// Blocks are removed by the page cascade, so a page can never be dropped while its blocks
    /// survive. `name` is matched by [`page_name_key`], so any spelling of the same page removes the
    /// one row it has. Returns the metadata of the committed revision, or `None` when the page had no
    /// rows, in which case nothing is written and the revision does not advance.
    pub fn remove_page(&mut self, name: &str) -> Result<Option<GraphMetadata>, IndexError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if find_page_id(&tx, &page_name_key(name))?.is_none() {
            return Ok(None);
        }
        tx.execute(
            "DELETE FROM pages WHERE name_key = ?1",
            [page_name_key(name)],
        )?;
        let metadata = advance_revision(&tx)?;
        tx.commit()?;
        Ok(Some(metadata))
    }

    /// Delete every page and every file baseline in one transaction.
    ///
    /// Used when the graph configuration changed: names, and therefore which page a file denotes,
    /// are derived from it, so every derived row of the old configuration is void and the files are
    /// projected again from scratch. Returns the metadata of the committed revision, or `None` when
    /// there was nothing to delete.
    pub fn clear_derived(&mut self) -> Result<Option<GraphMetadata>, IndexError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pages = tx.execute("DELETE FROM pages", [])?;
        let baselines = tx.execute("DELETE FROM file_baseline", [])?;
        tx.execute("UPDATE graph SET config_signature = NULL WHERE id = 1", [])?;
        if pages == 0 && baselines == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let metadata = advance_revision(&tx)?;
        tx.commit()?;
        Ok(Some(metadata))
    }
}

/// Advance the graph revision once inside an open write transaction.
///
/// Every committed write that changes product facts goes through here, so the returned revision is
/// the one that produced the rows the transaction just wrote or removed.
pub(crate) fn advance_revision(tx: &Transaction<'_>) -> Result<GraphMetadata, IndexError> {
    let current = schema::read_metadata(tx)?;
    let revision = current.revision() + 1;
    tx.execute("UPDATE graph SET revision = ?1 WHERE id = 1", [revision])?;
    Ok(GraphMetadata::new(
        current.instance_id().to_owned(),
        revision,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::PageDocument;
    use crate::index::test_support::*;
    use crate::index::MAX_BLOCK_DEPTH;
    use std::path::PathBuf;

    #[test]
    fn replacement_removes_the_previous_rows() {
        let mut index = Index::open_in_memory().expect("open index");
        // A later page keeps the replaced page's row key from being reused, so stale block rows
        // would survive as orphans instead of colliding with the new page.
        project(&mut index, "page", "- a\n\t- b\n\t- c\n").expect("first replacement");
        project(&mut index, "other", "- o\n").expect("replace other page");
        assert_eq!(row_counts(&index), (2, 4));

        project(&mut index, "page", "- only\n").expect("second replacement");
        assert_eq!(row_counts(&index), (2, 2));
    }

    #[test]
    fn one_page_keeps_one_row_whatever_its_spelling() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "Ä", "- old #old\n").expect("project Ä");
        assert_eq!(index.page_names().expect("read page names"), ["Ä"]);

        // The same file can state a new spelling. A different file claiming the key is rejected.
        assert!(project(&mut index, "ä", "- conflicting file\n").is_err());
        index
            .replace_page_with_baseline(
                Path::new("pages/Ä.md"),
                "ä",
                None,
                &PageDocument::parse("- new #new\n"),
                baseline(observed(1), 0x11),
            )
            .expect("replace the spelling on the same file");
        assert_eq!(
            index.page_names().expect("read page names"),
            ["ä"],
            "one page is one row, whatever its spelling"
        );
        assert_eq!(fact_rows(&index, TAGS, "ä"), ["0|new|new"]);

        // Removing by yet another spelling removes that same row, and a second removal is a miss
        // rather than a silent extra write.
        assert!(index.remove_page("Ä").expect("remove page").is_some());
        assert!(index.page_names().expect("read page names").is_empty());
        assert!(index.remove_page("ä").expect("remove again").is_none());
    }

    #[test]
    fn replacing_a_page_removes_the_previous_fact_rows() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "facts", FACTS_PAGE).expect("replace facts page");
        project(&mut index, "other", "- o #keep\n").expect("replace other page");
        assert_eq!(fact_row_counts(&index), [2, 1, 4, 1, 3]);

        project(&mut index, "facts", "title:: replaced\n\n- plain\n")
            .expect("replace facts page again");

        assert_eq!(
            fact_rows(&index, PAGE_PROPERTIES, "replaced"),
            ["0|title|title|replaced"]
        );
        for sql in [BLOCK_PROPERTIES, PAGE_REFS, BLOCK_REFS, TAGS] {
            assert_eq!(
                fact_rows(&index, sql, "replaced"),
                Vec::<String>::new(),
                "a replaced page must not keep the facts of its previous text"
            );
        }
        // The other page keeps its own facts: a cascade that removed too much shows up here.
        assert_eq!(fact_rows(&index, TAGS, "other"), ["0|keep|keep"]);
    }

    #[test]
    fn removing_a_page_removes_its_fact_rows() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "facts", FACTS_PAGE).expect("replace page");
        assert_eq!(fact_row_counts(&index), [2, 1, 4, 1, 2]);

        index.remove_page("Index slice").expect("remove page");

        assert_eq!(
            fact_row_counts(&index),
            [0, 0, 0, 0, 0],
            "the page cascade must reach every fact table"
        );
    }

    #[test]
    fn clearing_derived_state_removes_fact_rows() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "facts", FACTS_PAGE).expect("replace page");

        index.clear_derived().expect("clear derived state");

        assert_eq!(fact_row_counts(&index), [0, 0, 0, 0, 0]);
    }

    #[test]
    fn failed_replacement_keeps_the_previous_fact_rows() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "facts", FACTS_PAGE).expect("replace page");

        // Reject the second block of the next replacement so its transaction must roll back after
        // the page row and the first block's facts were already written.
        index
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_block BEFORE INSERT ON blocks
                 WHEN NEW.preorder = 1
                 BEGIN SELECT RAISE(ABORT, 'rejected by test'); END;",
            )
            .expect("install rejection trigger");
        project(&mut index, "facts", "- x #new [[New Page]]\n\t- y #new\n")
            .expect_err("replacement must fail");

        assert_eq!(
            fact_row_counts(&index),
            [2, 1, 4, 1, 2],
            "a rolled-back replacement must not leave a single new fact row behind"
        );
        assert_eq!(
            fact_rows(&index, PAGE_PROPERTIES, "Index slice"),
            ["0|title|title|Index slice", "1|Done_At|done-at|2026-10-29"]
        );
        assert_eq!(
            fact_rows(&index, TAGS, "Index slice"),
            ["0|tag|tag", "1|multi word|multi word"]
        );
    }

    #[test]
    fn failed_replacement_keeps_the_previous_rows() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "page", "- a\n\t- b\n").expect("first replacement");

        // Reject the second block of the next replacement so its transaction must roll back.
        index
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_block BEFORE INSERT ON blocks
                 WHEN NEW.preorder = 1
                 BEGIN SELECT RAISE(ABORT, 'rejected by test'); END;",
            )
            .expect("install rejection trigger");

        let error =
            project(&mut index, "page", "- x\n\t- y\n\t- z\n").expect_err("replacement must fail");
        assert!(matches!(error, IndexError::Sqlite(_)));

        assert_eq!(row_counts(&index), (1, 2));
        assert_eq!(index.metadata().expect("read metadata").revision(), 1);

        let tree = index
            .page_tree("page")
            .expect("query page tree")
            .expect("previous page survives");
        assert_eq!(tree.page().block_count(), 2);
        assert_eq!(tree.page().revision(), 1);
    }

    #[test]
    fn too_deep_page_is_rejected_before_any_row_is_written() {
        let mut index = Index::open_in_memory().expect("open index");
        let mut source = String::new();
        for depth in 0..=MAX_BLOCK_DEPTH + 1 {
            source.push_str(&"\t".repeat(depth as usize));
            source.push_str("- b\n");
        }

        let error = project(&mut index, "page", &source)
            .expect_err("a page deeper than the limit is rejected");

        match error {
            IndexError::TooDeep { depth, limit } => {
                assert_eq!(depth, MAX_BLOCK_DEPTH + 1);
                assert_eq!(limit, MAX_BLOCK_DEPTH);
            }
            other => panic!("expected a depth limit error, got {other:?}"),
        }
        assert_eq!(row_counts(&index), (0, 0));
        assert_eq!(index.metadata().expect("read metadata").revision(), 0);
    }

    #[test]
    fn removing_a_page_deletes_its_blocks_and_advances_the_revision_once() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "page", "- a\n\t- b\n").expect("replace page");
        project(&mut index, "other", "- o\n").expect("replace other page");
        assert_eq!(row_counts(&index), (2, 3));

        let metadata = index
            .remove_page("page")
            .expect("remove page")
            .expect("the page had rows");
        assert_eq!(metadata.revision(), 3);
        assert_eq!(
            row_counts(&index),
            (1, 1),
            "the page and its blocks are gone"
        );
        assert!(index
            .page_metadata("page")
            .expect("read metadata")
            .is_none());

        // Removing a page that has no rows writes nothing and leaves the revision alone.
        assert!(index.remove_page("page").expect("remove again").is_none());
        assert_eq!(index.metadata().expect("read metadata").revision(), 3);
    }

    #[test]
    fn a_page_outside_the_two_directories_cannot_be_projected() {
        // A page row states its kind, and only the two Logseq directories decide one. A caller that
        // hands in some other path gets the column named rather than a row whose kind no read could
        // trust, because `source::kind_of_path` answers `None` there.
        let mut index = Index::open_in_memory().expect("open index");
        let error = index
            .replace_page_with_baseline(
                Path::new("notes/alpha.md"),
                "alpha",
                None,
                &PageDocument::parse("- a\n"),
                baseline(observed(1), 0x11),
            )
            .expect_err("a path with no kind cannot be projected");
        assert!(matches!(
            error,
            IndexError::CorruptRow {
                column: "pages.relative_path",
                ..
            }
        ));
    }

    #[test]
    fn clearing_derived_state_removes_pages_and_baselines_in_one_write() {
        let mut index = Index::open_in_memory().expect("open index");
        let path = Path::new("pages/alpha.md");
        index
            .replace_page_with_baseline(
                path,
                "alpha",
                None,
                &PageDocument::parse("- a\n"),
                baseline(observed(1), 0x11),
            )
            .expect("project a page");
        assert_eq!(row_counts(&index), (1, 1));

        let metadata = index
            .clear_derived()
            .expect("clear derived state")
            .expect("there was state to clear");
        assert_eq!(metadata.revision(), 2);
        assert_eq!(row_counts(&index), (0, 0));
        assert!(index.file_baseline(path).expect("read baseline").is_none());
        assert!(index.page_names().expect("read page names").is_empty());
        assert!(index
            .file_baseline_paths()
            .expect("read baseline paths")
            .is_empty());

        // Nothing left to delete means nothing is written.
        assert!(index.clear_derived().expect("clear again").is_none());
        assert_eq!(index.metadata().expect("read metadata").revision(), 2);
    }

    #[test]
    fn a_page_and_its_baseline_commit_together() {
        let mut index = Index::open_in_memory().expect("open index");
        let path = Path::new("pages/alpha.md");

        index
            .replace_page_with_baseline(
                path,
                "alpha",
                None,
                &PageDocument::parse("- a\n\t- b\n"),
                baseline(observed(9), 0x1234),
            )
            .expect("replace page with baseline");

        let stored = index
            .file_baseline(path)
            .expect("read baseline")
            .expect("baseline exists");
        assert_eq!(stored.metadata(), observed(9));
        assert_eq!(stored.content_hash(), 0x1234);

        // A rejected replacement must leave neither the new page nor its baseline behind.
        index
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_block BEFORE INSERT ON blocks
                 WHEN NEW.preorder = 1
                 BEGIN SELECT RAISE(ABORT, 'rejected by test'); END;",
            )
            .expect("install rejection trigger");
        index
            .replace_page_with_baseline(
                path,
                "alpha",
                None,
                &PageDocument::parse("- x\n\t- y\n"),
                baseline(observed(11), 0x9999),
            )
            .expect_err("replacement must fail");

        let stored = index
            .file_baseline(path)
            .expect("read baseline")
            .expect("baseline survives");
        assert_eq!(stored.metadata(), observed(9));
        assert_eq!(stored.content_hash(), 0x1234);
        assert_eq!(index.metadata().expect("read metadata").revision(), 1);
    }

    #[test]
    fn a_baseline_can_be_refreshed_without_advancing_the_revision() {
        let mut index = Index::open_in_memory().expect("open index");
        let path = Path::new("pages/alpha.md");
        index
            .replace_page_with_baseline(
                path,
                "alpha",
                None,
                &PageDocument::parse("- a\n"),
                baseline(observed(4), 0xabcd),
            )
            .expect("replace page with baseline");

        index
            .record_file_baseline(path, baseline(observed(4), 0xabcd))
            .expect("refresh baseline");

        assert_eq!(index.metadata().expect("read metadata").revision(), 1);
        assert_eq!(
            index
                .file_baseline(path)
                .expect("read baseline")
                .expect("baseline exists")
                .content_hash(),
            0xabcd
        );
    }

    #[test]
    fn a_missing_baseline_is_absent_rather_than_defaulted() {
        let index = Index::open_in_memory().expect("open index");
        assert!(index
            .file_baseline(Path::new("pages/unknown.md"))
            .expect("read baseline")
            .is_none());
    }

    #[test]
    fn the_multi_page_commit_replaces_both_pages_and_advances_the_revision_once() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "from", "- a #old [[Old]]\n\t- child\n").expect("project from");
        project(&mut index, "to", "- b #stays\n").expect("project to");
        let before = index.metadata().expect("read metadata").revision();
        assert_eq!(before, 2);

        let metadata = index
            .replace_pages_with_baselines(&[
                (
                    PathBuf::from("pages/from.md"),
                    "from".to_owned(),
                    None,
                    PageDocument::parse("- kept\n"),
                    baseline(observed(6), 0xaaaa),
                ),
                (
                    PathBuf::from("pages/to.md"),
                    "to".to_owned(),
                    None,
                    PageDocument::parse("- b #stays\n- moved #new [[New]]\n\t- carried\n"),
                    baseline(observed(34), 0xbbbb),
                ),
            ])
            .expect("commit both pages");

        // One revision advance for the pair, not one per page.
        assert_eq!(metadata.revision(), before + 1);
        assert_eq!(
            index.metadata().expect("read metadata").revision(),
            before + 1
        );

        // Both pages were replaced as wholes: the old facts are gone and the new ones are in,
        // each on its own page.
        assert_eq!(fact_rows(&index, TAGS, "from"), Vec::<String>::new());
        assert_eq!(
            fact_rows(&index, TAGS, "to"),
            ["0|stays|stays", "1|new|new"]
        );
        assert_eq!(row_counts(&index), (2, 4));
        assert_eq!(
            index
                .file_baseline(Path::new("pages/to.md"))
                .expect("read baseline")
                .expect("baseline exists")
                .content_hash(),
            0xbbbb
        );
    }

    #[test]
    fn a_failed_multi_page_commit_keeps_every_page() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "from", "- a #old\n").expect("project from");
        project(&mut index, "to", "- b #old\n").expect("project to");

        // Reject the second page so the transaction must roll back after the first page was
        // already written inside it.
        index
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_to_page BEFORE INSERT ON pages
                 WHEN NEW.name = 'to'
                 BEGIN SELECT RAISE(ABORT, 'rejected by test'); END;",
            )
            .expect("install rejection trigger");
        index
            .replace_pages_with_baselines(&[
                (
                    PathBuf::from("pages/from.md"),
                    "from".to_owned(),
                    None,
                    PageDocument::parse("- changed\n"),
                    baseline(observed(9), 0xdddd),
                ),
                (
                    PathBuf::from("pages/to.md"),
                    "to".to_owned(),
                    None,
                    PageDocument::parse("- changed too\n"),
                    baseline(observed(13), 0xeeee),
                ),
            ])
            .expect_err("the second page must fail the commit");

        assert_eq!(
            fact_rows(&index, TAGS, "from"),
            ["0|old|old"],
            "the first page's replacement rolled back with the rest"
        );
        assert_eq!(
            fact_rows(&index, TAGS, "to"),
            ["0|old|old"],
            "neither page took the new rows"
        );
    }
}
