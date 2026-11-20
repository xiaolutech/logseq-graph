#[cfg(doc)]
use super::block::BLOCK_BY_DURABLE_ID;
#[cfg(doc)]
use super::facts::{backlinks, pages_with_tag};
use super::row_selector;
use crate::domain::{
    page_name_key, BlockMarker, BlockNode, BlockView, JournalDate, PageMetadata, PageSummary,
    PageTree,
};
use crate::index::{decode_marker, decode_page_kind, IndexError, MAX_BLOCK_DEPTH};
use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;

/// Metadata of one page, or `None` when the page has no derived rows.
///
/// The page is resolved by [`page_name_key`], so any spelling of the page's name answers for it.
pub(in crate::index) fn page_metadata(
    conn: &Connection,
    name: &str,
) -> Result<Option<PageMetadata>, IndexError> {
    Ok(page_row(conn, name)?.map(|(_, metadata)| metadata))
}

/// Graph-relative path of the file the page `name` was projected from, or `None` when it has no row.
///
/// The page is resolved by [`page_name_key`] like every other page read, and the path is the one
/// `pages.relative_path` stores: the column the projection writes together with the file baseline,
/// so this is a single seek on `pages.name_key` and no join is needed.
///
/// The caller re-establishes the file from that path. If the file is no longer there the row is
/// stale, which is what tells the caller to discover the graph again instead of trusting it.
pub(in crate::index) fn page_file(
    conn: &Connection,
    name: &str,
) -> Result<Option<PathBuf>, IndexError> {
    let stored = conn
        .query_row(
            "SELECT relative_path FROM pages WHERE name_key = ?1",
            [page_name_key(name)],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(stored.map(PathBuf::from))
}

/// One page and its complete block tree, or `None` when the page has no derived rows.
///
/// The page is resolved by [`page_name_key`], so any spelling of the page's name answers for it.
/// Both statements read inside one deferred transaction: the page row and its blocks must come from
/// one snapshot, or a page replaced in between would be reported with its old `block_count` and its
/// new blocks (or the reverse).
pub(in crate::index) fn page_tree(
    conn: &Connection,
    name: &str,
) -> Result<Option<PageTree>, IndexError> {
    let tx = super::super::ReadTransaction::new(conn)?;
    let Some((page_id, metadata)) = page_row(&tx, name)? else {
        return Ok(None);
    };
    let blocks = match page_id {
        Some(id) => page_blocks(&tx, id)?,
        None => Vec::new(),
    };
    tx.commit()?;
    Ok(Some(PageTree::new(metadata, blocks)))
}

/// Every page with its block count and, for a journal, its day, ordered by folded page name.
///
/// The order is `pages.name_key`, the same folded order [`backlinks`] and [`pages_with_tag`] use, so
/// every list of page names in this layer comes back in one order. Both kinds are listed by this one
/// read: a caller filters on [`PageSummary::kind`] instead of issuing a second query, and the rows
/// arrive in the same order either way.
///
/// The order deliberately stays the folded name, which for a journal is a rendering rather than a
/// date: ordering by `journal_date` here would interleave two kinds by a column only one of them
/// has, and put every regular page before or after every journal. A caller that wants chronology
/// sorts the rows it kept with [`PageSummary::journal_date`], whose `YYYY-MM-DD` spelling sorts in
/// time order by itself.
const PAGE_SUMMARIES: &str = "\
SELECT name, kind, journal_date, block_count, relative_path IS NOT NULL
  FROM page_entities
 ORDER BY name_key";

/// Every page with its block count, ordered by folded page name.
pub(in crate::index) fn pages(conn: &Connection) -> Result<Vec<PageSummary>, IndexError> {
    let mut stmt = conn.prepare(PAGE_SUMMARIES)?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        // Two short columns beyond the block count: no extra seek, no extra table. Measured in
        // PERFORMANCE.md, because a listing reads one row per page.
        let journal_date: Option<String> = row.get(2)?;
        out.push(PageSummary::new(
            row.get(0)?,
            decode_page_kind(&row.get::<_, String>(1)?, "pages.kind")?,
            decode_journal_date(journal_date, "pages.journal_date")?,
            row.get(3)?,
            row.get(4)?,
        ));
    }
    Ok(out)
}

/// Every block of one page in preorder order, with its text.
///
/// The parent's preorder follows by primary key, like [`BLOCK_BY_DURABLE_ID`], so a block that
/// reports its parent costs one key lookup rather than another read. The rows come back ordered by
/// `blocks.preorder`, which is unique within a page.
///
/// The selector is decided from what the row states, not from a selector, because no selector asked
/// for this read: a block that states an `id::` is answered as `Durable`, and a block without one as
/// `Ephemeral` with the id the projection minted for it (ADR-0012).
pub(super) const PAGE_BLOCK_VIEWS: &str = "\
SELECT blocks.preorder,
       blocks.depth,
       parent.preorder,
       blocks.text,
       blocks.durable_id,
       blocks.ephemeral_id,
       blocks.marker_kind,
       blocks.heading_level
  FROM blocks
  LEFT JOIN blocks AS parent ON parent.id = blocks.parent_id
 WHERE blocks.page_id = ?1
 ORDER BY blocks.preorder";

/// Every block of one page in preorder order, or `None` when the page has no derived rows.
///
/// The page is resolved by [`page_name_key`], so any spelling of its name answers for it, and the
/// returned views carry the stored spelling. The page row and the block rows read in one deferred
/// transaction, so they describe one revision.
pub(in crate::index) fn page_block_views(
    conn: &Connection,
    name: &str,
) -> Result<Option<Vec<BlockView>>, IndexError> {
    let tx = super::super::ReadTransaction::new(conn)?;
    let Some((page_id, metadata)) = page_row(&tx, name)? else {
        return Ok(None);
    };
    let blocks = match page_id {
        Some(id) => block_views(&tx, id, metadata.name())?,
        None => Vec::new(),
    };
    tx.commit()?;
    Ok(Some(blocks))
}

fn block_views(
    conn: &Connection,
    page_id: i64,
    page_name: &str,
) -> Result<Vec<BlockView>, IndexError> {
    let mut stmt = conn.prepare(PAGE_BLOCK_VIEWS)?;
    let mut rows = stmt.query([page_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(BlockView::new(
            page_name.to_owned(),
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            decode_marker(&row.get::<_, String>(6)?, row.get::<_, Option<u32>>(7)?)?,
            row_selector(row.get(4)?, row.get(5)?)?,
        ));
    }
    Ok(out)
}

/// Every stored page name, ordered by name.
///
/// A scan compares this list with the names discovery returned, so a page whose file was deleted or
/// renamed loses its rows instead of staying readable forever. Page rows are matched by name rather
/// than by path because `pages` is keyed by name; the file baseline is the table keyed by path.
pub(in crate::index) fn page_names(conn: &Connection) -> Result<Vec<String>, IndexError> {
    let mut stmt = conn.prepare("SELECT name FROM pages ORDER BY name")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row.get(0)?);
    }
    Ok(out)
}

/// The `pages` row for one page, addressed by its identity rather than by its spelling.
///
/// `name_key` is the filter, so `Ä` and `ä` reach one row and no read has to repeat the name rule.
/// The returned metadata keeps the spelling `pages.name` stores, which is what a caller sees as the
/// page's name.
pub(super) const PAGE_ROW: &str = "\
SELECT id, name, kind, journal_date, block_count, revision
  FROM pages
 WHERE name_key = ?1
 UNION ALL
SELECT NULL, MIN(claim.name), 'page', NULL, 0, MAX(owner.revision)
  FROM page_claims AS claim JOIN pages AS owner ON owner.id = claim.page_id
 WHERE claim.name_key = ?1 AND NOT EXISTS (SELECT 1 FROM pages WHERE name_key = ?1)
 GROUP BY claim.name_key";

/// Page row with its private integer key, which stays inside the index.
///
/// Addressed by [`page_name_key`], not by the spelling the caller used: a page name is an identity,
/// so `ä` must find the page stored as `Ä` exactly as an exact spelling would. An exact match on
/// `pages.name` would be a BINARY comparison against a column that exists only to be handed back.
pub(super) fn page_row(
    conn: &Connection,
    name: &str,
) -> Result<Option<(Option<i64>, PageMetadata)>, IndexError> {
    let row = conn
        .query_row(PAGE_ROW, [page_name_key(name)], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, u64>(5)?,
            ))
        })
        .optional()?;
    let Some((id, name, kind, journal_date, block_count, revision)) = row else {
        return Ok(None);
    };
    // The kind and the day are decoded outside the row closure: `rusqlite` owns the closure's error
    // type, and a value this build cannot read back is this crate's error, not a driver failure.
    let kind = decode_page_kind(&kind, "pages.kind")?;
    let journal_date = decode_journal_date(journal_date, "pages.journal_date")?;
    Ok(Some((
        id,
        PageMetadata::new(
            name,
            kind,
            journal_date,
            block_count,
            revision,
            id.is_some(),
        ),
    )))
}

/// The calendar day a `pages.journal_date` value states, or `None` when the page is not a journal.
///
/// The column has one spelling, so anything else is a row this build cannot read: reporting it is
/// the only answer that is not a guess, and a substituted day would make an ordering quietly wrong
/// instead of loudly broken.
fn decode_journal_date(
    stored: Option<String>,
    column: &'static str,
) -> Result<Option<JournalDate>, IndexError> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    JournalDate::parse_stored(&stored)
        .map(Some)
        .ok_or(IndexError::CorruptRow {
            column,
            value: stored,
        })
}

/// One stored block, still flat and in preorder.
///
/// Deliberately not `Copy` or comparable: the only consumer moves rows out of the flat list while
/// it rebuilds the tree, and no code compares two rows.
struct FlatBlock {
    preorder: u32,
    depth: u32,
    sibling_index: u32,
    marker: BlockMarker,
}

pub(super) fn page_blocks(conn: &Connection, page_id: i64) -> Result<Vec<BlockNode>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT preorder, depth, sibling_index, marker_kind, heading_level
           FROM blocks WHERE page_id = ?1 ORDER BY preorder",
    )?;
    let mut rows = stmt.query([page_id])?;

    let mut flat = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: String = row.get(3)?;
        let level: Option<u32> = row.get(4)?;
        flat.push(FlatBlock {
            preorder: row.get(0)?,
            depth: row.get(1)?,
            sibling_index: row.get(2)?,
            marker: decode_marker(&kind, level)?,
        });
    }

    build_tree(flat)
}

/// A block whose children are still being collected.
struct OpenBlock {
    block: FlatBlock,
    children: Vec<BlockNode>,
}

impl OpenBlock {
    fn finish(self) -> BlockNode {
        let Self { block, children } = self;
        BlockNode::new(
            block.preorder,
            block.depth,
            block.sibling_index,
            block.marker,
            children,
        )
    }
}

/// Rebuild the tree from rows ordered by preorder.
///
/// Each row is either the first child of the block currently being collected or a sibling of an
/// already open block, so the open ancestors form a stack that is popped until the next row's
/// parent is on top. Depth is checked against the stored parent, which keeps the produced tree
/// within [`MAX_BLOCK_DEPTH`] and therefore bounds the recursive `BlockNode` drop.
fn build_tree(flat: Vec<FlatBlock>) -> Result<Vec<BlockNode>, IndexError> {
    let mut roots = Vec::new();
    let mut open: Vec<OpenBlock> = Vec::new();

    for block in flat {
        if block.depth > MAX_BLOCK_DEPTH {
            return Err(IndexError::CorruptRow {
                column: "blocks.depth",
                value: format!(
                    "depth {} above the supported limit {MAX_BLOCK_DEPTH}",
                    block.depth
                ),
            });
        }

        while open
            .last()
            .is_some_and(|frame| frame.block.depth >= block.depth)
        {
            close_top(&mut open, &mut roots);
        }

        let expected_depth = open.last().map_or(0, |frame| frame.block.depth + 1);
        if block.depth != expected_depth {
            return Err(IndexError::CorruptRow {
                column: "blocks.depth",
                value: format!(
                    "depth {} where depth {expected_depth} was expected",
                    block.depth
                ),
            });
        }

        open.push(OpenBlock {
            block,
            children: Vec::new(),
        });
    }

    while !open.is_empty() {
        close_top(&mut open, &mut roots);
    }

    Ok(roots)
}

/// Finish the innermost open block and attach it to its parent, or to the roots.
///
/// A caller owns the test that the stack is non-empty: `build_tree` reaches the first call site
/// under `open.last()` being `Some`, and the second under `!open.is_empty()`.
fn close_top(open: &mut Vec<OpenBlock>, roots: &mut Vec<BlockNode>) {
    #[expect(
        clippy::expect_used,
        reason = "both call sites in `build_tree` test `open.last()` / `!open.is_empty()` \
                  immediately before calling, and nothing here pops more than the one frame"
    )]
    let frame = open.pop().expect("caller opened a frame");
    let node = frame.finish();
    match open.last_mut() {
        Some(parent) => parent.children.push(node),
        None => roots.push(node),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bullet(preorder: u32, depth: u32, sibling_index: u32) -> FlatBlock {
        FlatBlock {
            preorder,
            depth,
            sibling_index,
            marker: BlockMarker::Bullet,
        }
    }

    #[test]
    fn empty_page_builds_an_empty_tree() {
        assert!(build_tree(Vec::new()).expect("empty tree").is_empty());
    }

    #[test]
    fn skipped_depth_is_rejected() {
        let error = build_tree(vec![bullet(0, 0, 0), bullet(1, 2, 0)])
            .expect_err("a child two levels below its parent is corrupt");
        assert!(matches!(
            error,
            IndexError::CorruptRow {
                column: "blocks.depth",
                ..
            }
        ));
    }

    #[test]
    fn depth_above_the_limit_is_rejected() {
        let rows = (0..=MAX_BLOCK_DEPTH + 1)
            .map(|depth| bullet(depth, depth, 0))
            .collect();
        let error = build_tree(rows).expect_err("a tree deeper than the limit is corrupt");
        assert!(matches!(
            error,
            IndexError::CorruptRow {
                column: "blocks.depth",
                ..
            }
        ));
    }

    #[test]
    fn impossible_marker_combinations_are_rejected() {
        for (kind, level) in [("bullet", Some(2)), ("heading", None), ("heading", Some(0))] {
            let error = decode_marker(kind, level).expect_err("marker combination is corrupt");
            assert!(matches!(
                error,
                IndexError::CorruptRow {
                    column: "blocks.marker_kind",
                    ..
                }
            ));
        }
    }

    #[test]
    fn a_stored_journal_day_is_decoded_or_reported_corrupt() {
        // The column has one spelling, so a value that does not read back is a row this build
        // cannot answer for. Guessing a day would make an ordering quietly wrong; the error names
        // the column instead.
        assert_eq!(
            decode_journal_date(Some("2026-08-13".to_owned()), "pages.journal_date")
                .expect("the stored spelling reads back"),
            JournalDate::new(2026, 8, 13)
        );
        assert_eq!(
            decode_journal_date(None, "pages.journal_date").expect("a page states no day"),
            None
        );
        for stored in [
            "2026-02-30",
            "2026-8-13",
            "2026-08-13T00:00",
            "",
            "not a date",
        ] {
            let error = decode_journal_date(Some(stored.to_owned()), "pages.journal_date")
                .expect_err("an unreadable day is corrupt state");
            assert!(
                matches!(
                    error,
                    IndexError::CorruptRow {
                        column: "pages.journal_date",
                        ..
                    }
                ),
                "{stored:?} must report the column, got {error:?}"
            );
        }
    }
}
