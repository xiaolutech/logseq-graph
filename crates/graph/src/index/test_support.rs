use super::{find_page_id, FileBaseline, Index, IndexError};
use crate::document::PageDocument;
use crate::domain::GraphMetadata;
use crate::source::{content_fingerprint, FileMetadata};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

/// Project `source` as the page `name`, with the baseline of the file it would have come from.
///
/// [`Index::replace_page_with_baseline`] is the only way to write a page: a page row carries the
/// path of the file it was projected from, so a test that only has text states that path and the
/// observation here. The path follows the name and the size and fingerprint come from the source,
/// so the baseline describes exactly the text that was parsed. The result is passed through, so a
/// test that expects the projection to fail can still assert on the error.
pub(super) fn project(
    index: &mut Index,
    name: &str,
    source: &str,
) -> Result<GraphMetadata, IndexError> {
    let baseline = FileBaseline::new(
        FileMetadata::from_stored(source.len() as u64, None),
        content_fingerprint(source),
    );
    let relative_path = PathBuf::from(format!("pages/{name}.md"));
    index.replace_page_with_baseline(
        &relative_path,
        name,
        None,
        &PageDocument::parse(source),
        baseline,
    )
}

pub(super) fn count(index: &Index, table: &str) -> i64 {
    index
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count rows")
}

pub(super) fn row_counts(index: &Index) -> (i64, i64) {
    (count(index, "pages"), count(index, "blocks"))
}

/// Compare every stored `parent_id` with the parent implied by `depth` and `preorder`.
///
/// The page tree read rebuilds the hierarchy from `depth` alone, so `parent_id` currently has
/// no production reader and two copies of the same fact could drift apart unnoticed. Returns
/// one message per block whose stored link disagrees with the implied one.
pub(super) fn parent_linkage_mismatches(index: &Index, name: &str) -> Vec<String> {
    let page_id = find_page_id(&index.conn, name)
        .expect("find page")
        .expect("page exists");
    let mut stmt = index
        .conn
        .prepare(
            "SELECT id, parent_id, preorder, depth
               FROM blocks WHERE page_id = ?1 ORDER BY preorder",
        )
        .expect("prepare block query");
    let rows = stmt
        .query_map([page_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, u32>(3)?,
            ))
        })
        .expect("query blocks")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read blocks");

    // Blocks arrive in preorder, so the open ancestors of the next block are the tail of a
    // stack popped until a strictly shallower block is on top.
    let mut open: Vec<(u32, i64)> = Vec::new();
    let mut mismatches = Vec::new();
    for (id, parent_id, preorder, depth) in rows {
        while open
            .last()
            .is_some_and(|(open_depth, _)| *open_depth >= depth)
        {
            open.pop();
        }
        let implied = open.last().map(|(_, id)| *id);
        if parent_id != implied {
            mismatches.push(format!(
                "block at preorder {preorder} (depth {depth}) stores parent {parent_id:?}, \
                 but its depth implies {implied:?}"
            ));
        }
        open.push((depth, id));
    }
    mismatches
}

/// Rows one of the fact queries returns for `name`, joined into one string per row.
///
/// A fact row has no public read path yet, so these assertions read the stored row directly.
/// The query API is the next slice; until then a test is the only place that may name these
/// tables, and it names them here instead of in an integration test.
pub(super) fn fact_rows(index: &Index, sql: &str, name: &str) -> Vec<String> {
    let mut stmt = index.conn.prepare(sql).expect("prepare fact query");
    let rows = stmt
        .query_map([name], |row| row.get::<_, String>(0))
        .expect("query fact rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read fact rows");
    rows
}

/// Rows one query with no bound parameter returns, joined into one string per row.
pub(super) fn all_rows(index: &Index, sql: &str) -> Vec<String> {
    let mut stmt = index.conn.prepare(sql).expect("prepare query");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read rows");
    rows
}

/// Number of rows in each fact table, in the order page properties, block properties, page refs,
/// block refs, tags.
///
/// Counted without a join, so a row orphaned by a page or block removal still shows up. The
/// joined queries cannot see an orphan: its page is gone, so nothing matches it.
pub(super) fn fact_row_counts(index: &Index) -> [i64; 5] {
    [
        count(index, "page_properties"),
        count(index, "block_properties"),
        count(index, "page_refs"),
        count(index, "block_refs"),
        count(index, "tags"),
    ]
}

/// A temporary directory that removes itself when the test ends.
pub(super) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(super) fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "logseq-graph-index-{}-{label}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    pub(super) fn file(&self) -> PathBuf {
        // A nested path, so opening also proves the directory is created.
        self.path.join("cache").join("index.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(super) fn baseline(metadata: FileMetadata, content_hash: u64) -> FileBaseline {
    FileBaseline::new(metadata, content_hash)
}

pub(super) fn observed(size: u64) -> FileMetadata {
    FileMetadata::from_stored(size, Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)))
}

/// Read the `application_id` recorded in a database file.
pub(super) fn application_id(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "application_id", |row| row.get(0))
        .expect("read application id")
}

/// Names of the user tables stored in a database file.
pub(super) fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare table query");
    stmt.query_map([], |row| row.get::<_, String>(0))
        .expect("query tables")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read tables")
}

/// The `durable_id` of the block at `preorder` of `name`, distinguishing NULL from a value.
pub(super) fn durable_id_at(index: &Index, name: &str, preorder: u32) -> Option<String> {
    index
        .conn
        .query_row(
            "SELECT blocks.durable_id
               FROM blocks
               JOIN pages ON pages.id = blocks.page_id
              WHERE pages.name = ?1 AND blocks.preorder = ?2",
            params![name, preorder],
            |row| row.get(0),
        )
        .expect("read durable_id")
}

/// Number of block rows that state a durable id, at every page.
pub(super) fn blocks_with_durable_id(index: &Index) -> i64 {
    index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM blocks WHERE durable_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("count durable ids")
}

/// A page with every kind of content fact: page properties, a block property, a tag, a page ref
/// with an alias-free name, a namespace ref that also references its parent, a block ref in
/// upper case, and a bracketed tag.
pub(super) const FACTS_PAGE: &str = "\
title:: Index slice
Done_At:: 2026-10-29

- root #tag [[Linked Page]] ((61745DEB-EEED-4426-8020-EB9E5C90D558))
  id:: 11111111-2222-3333-4444-555555555555
\t- child #[[multi word]] [[a/b]]
";

/// Page property rows of `name`, as `ordinal|key|normalized_key|value`.
pub(super) const PAGE_PROPERTIES: &str = "\
SELECT ordinal || '|' || key || '|' || normalized_key || '|' || value
  FROM page_properties
  JOIN pages ON pages.id = page_properties.page_id
 WHERE pages.name = ?1
 ORDER BY ordinal";

/// Block property rows of `name`, as `preorder|ordinal|key|normalized_key|value`.
pub(super) const BLOCK_PROPERTIES: &str = "\
SELECT blocks.preorder || '|' || ordinal || '|' || key || '|' || normalized_key || '|' || value
  FROM block_properties
  JOIN blocks ON blocks.id = block_properties.block_id
  JOIN pages ON pages.id = blocks.page_id
 WHERE pages.name = ?1
 ORDER BY blocks.preorder, ordinal";

/// Page ref rows of `name`, as `preorder|target_name|target_key`.
pub(super) const PAGE_REFS: &str = "\
SELECT blocks.preorder || '|' || target_name || '|' || target_key
  FROM page_refs
  JOIN blocks ON blocks.id = page_refs.block_id
  JOIN pages ON pages.id = blocks.page_id
 WHERE pages.name = ?1
 ORDER BY blocks.preorder, target_name";

/// Block ref rows of `name`, as `preorder|target_uuid`.
pub(super) const BLOCK_REFS: &str = "\
SELECT blocks.preorder || '|' || target_uuid
  FROM block_refs
  JOIN blocks ON blocks.id = block_refs.block_id
  JOIN pages ON pages.id = blocks.page_id
 WHERE pages.name = ?1
 ORDER BY blocks.preorder, target_uuid";

/// Tag rows of `name`, as `preorder|name|name_key`.
pub(super) const TAGS: &str = "\
SELECT blocks.preorder || '|' || tags.name || '|' || tags.name_key
  FROM tags
  JOIN blocks ON blocks.id = tags.block_id
  JOIN pages ON pages.id = blocks.page_id
 WHERE pages.name = ?1
 ORDER BY blocks.preorder, tags.name";

/// Block task state and priority of `name`, as `preorder|task_marker|priority`, with `-` for a
/// block that states neither.
pub(super) const TASK_FACTS: &str = "\
SELECT blocks.preorder
   || '|' || COALESCE(blocks.task_marker, '-')
   || '|' || COALESCE(blocks.priority, '-')
  FROM blocks
  JOIN pages ON pages.id = blocks.page_id
 WHERE pages.name = ?1
 ORDER BY blocks.preorder";
