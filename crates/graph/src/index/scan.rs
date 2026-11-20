//! Batch scan commits, retaining one revision advance per replaced or removed page.

use std::path::PathBuf;

use rusqlite::TransactionBehavior;

use crate::domain::JournalDate;

use super::projection::{write_prepared_page, ProjectedPage};
use super::{advance_revision, baseline, FileBaseline, Index, IndexError};

#[derive(Debug)]
pub(crate) struct ScanPage {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) date: Option<JournalDate>,
    pub(crate) baseline: FileBaseline,
    pub(crate) projection: Option<ProjectedPage>,
}

impl Index {
    pub(crate) fn snapshot_shadow_identities(&self) -> Result<(), IndexError> {
        self.conn.execute_batch("CREATE TEMP TABLE shadow_identity AS SELECT p.relative_path, b.preorder, b.ephemeral_id, f.content_hash FROM blocks b JOIN pages p ON p.id = b.page_id JOIN file_baseline f ON f.relative_path = p.relative_path WHERE b.ephemeral_id IS NOT NULL")?;
        Ok(())
    }

    pub(crate) fn restore_shadow_identities(&mut self) -> Result<(), IndexError> {
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE blocks AS b SET ephemeral_id = s.ephemeral_id FROM pages p JOIN file_baseline f ON f.relative_path = p.relative_path JOIN shadow_identity s ON s.relative_path = p.relative_path AND s.content_hash = f.content_hash WHERE b.page_id = p.id AND b.preorder = s.preorder AND b.ephemeral_id IS NOT NULL", [])?;
        tx.execute_batch("DROP TABLE shadow_identity")?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn finish_shadow(
        &mut self,
        live: &Index,
        signature: &str,
    ) -> Result<(), IndexError> {
        let latest = live.metadata()?;
        let next = latest
            .revision()
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| IndexError::CorruptRow {
                column: "graph.revision",
                value: "revision exhausted".to_owned(),
            })?;
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE pages SET revision = ?1 WHERE revision < ?1", [next])?;
        tx.execute("UPDATE graph SET instance_id = ?1, revision = max(revision, ?2), config_signature = ?3 WHERE id = 1", rusqlite::params![latest.instance_id(), next, signature])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn preserve_identities(
        &mut self,
        live: &Index,
        path: &std::path::Path,
    ) -> Result<(), IndexError> {
        let Some(baseline) = self.file_baseline(path)? else {
            return Ok(());
        };
        if live
            .file_baseline(path)?
            .is_none_or(|other| other.content_hash() != baseline.content_hash())
        {
            return Ok(());
        }
        let path = baseline::path_key(path)?;
        let mut select = live.conn.prepare_cached("SELECT b.preorder, b.ephemeral_id FROM blocks b JOIN pages p ON p.id = b.page_id WHERE p.relative_path = ?1 AND b.ephemeral_id IS NOT NULL")?;
        let rows = select.query_map([&path], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
        })?;
        let tx = self.conn.transaction()?;
        for row in rows {
            let (preorder, id) = row?;
            tx.execute("UPDATE blocks SET ephemeral_id = ?1 WHERE page_id = (SELECT id FROM pages WHERE relative_path = ?2) AND preorder = ?3 AND durable_id IS NULL", rusqlite::params![id, path, preorder])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn commit_scan(
        &mut self,
        signature: &str,
        clear: bool,
        removed: &[PathBuf],
        pages: &[ScanPage],
    ) -> Result<(), IndexError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if clear {
            let pages = tx.execute("DELETE FROM pages", [])?;
            let baselines = tx.execute("DELETE FROM file_baseline", [])?;
            if pages != 0 || baselines != 0 {
                advance_revision(&tx)?;
            }
        } else {
            for path in removed {
                let path = baseline::path_key(path)?;
                if tx.execute("DELETE FROM pages WHERE relative_path = ?1", [&path])? != 0 {
                    advance_revision(&tx)?;
                }
                tx.execute(
                    "DELETE FROM file_baseline WHERE relative_path = ?1",
                    [&path],
                )?;
            }
        }
        for page in pages {
            if let Some(projection) = &page.projection {
                let metadata = advance_revision(&tx)?;
                write_prepared_page(
                    &tx,
                    &page.path,
                    &page.name,
                    page.date,
                    projection,
                    page.baseline,
                    metadata.revision(),
                )?;
            } else {
                baseline::write(&tx, &page.path, page.baseline)?;
            }
        }
        tx.execute(
            "UPDATE graph SET config_signature = ?1 WHERE id = 1",
            [signature],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::PageDocument;
    use crate::source::{content_fingerprint, FileMetadata};

    fn page(name: &str, text: &str) -> ScanPage {
        ScanPage {
            path: PathBuf::from(format!("pages/{name}.md")),
            name: name.to_owned(),
            date: None,
            baseline: FileBaseline::new(
                FileMetadata::from_stored(text.len() as u64, None),
                content_fingerprint(text),
            ),
            projection: Some(
                ProjectedPage::project(&PageDocument::parse(text)).expect("projection"),
            ),
        }
    }

    #[test]
    fn a_late_failure_rolls_back_earlier_pages_and_the_configuration() {
        let mut index = Index::open_in_memory().expect("index");
        index
            .commit_scan(
                "before",
                false,
                &[],
                &[page("alpha", "- old\n"), page("beta", "- old\n")],
            )
            .expect("initial batch");
        let revision = index.metadata().expect("metadata").revision();
        index.conn.execute_batch("CREATE TRIGGER reject_second_page BEFORE INSERT ON blocks WHEN NEW.text = 'reject' BEGIN SELECT RAISE(ABORT, 'second page rejected'); END;").expect("trigger");
        assert!(index
            .commit_scan(
                "after",
                true,
                &[],
                &[
                    page("alpha", "- new\n- second\n"),
                    page("beta", "- reject\n")
                ]
            )
            .is_err());
        assert_eq!(index.metadata().expect("metadata").revision(), revision);
        assert_eq!(
            index
                .page_metadata("alpha")
                .expect("metadata")
                .expect("alpha")
                .block_count(),
            1
        );
        assert!(index
            .configuration_matches("before")
            .expect("old signature"));
        assert!(!index.configuration_matches("after").expect("new signature"));
    }
}
