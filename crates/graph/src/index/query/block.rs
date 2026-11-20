#[cfg(doc)]
use super::page::PAGE_BLOCK_VIEWS;
use super::page::{page_blocks, page_row};
use super::row_selector;
use crate::domain::{page_name_key, BlockId, BlockSiblings, BlockView, EphemeralId};
#[cfg(doc)]
use crate::index::MAX_BLOCK_DEPTH;
use crate::index::{
    decode_marker, DurableLookup, IndexError, LocatedBlock, LocatedEphemeralBlock, LocatedPageBlock,
};
use rusqlite::{params, Connection, OptionalExtension};

/// Where one block sits among its siblings, with its page's own key.
///
/// The previous and the next sibling are two questions about the same row, so both reads start here:
/// the page's integer id, its stored name, the block's `parent_id` (NULL for a top-level block), and
/// the block's own `sibling_index`.
const SIBLING_CONTEXT: &str = "\
SELECT pages.id, pages.name, blocks.parent_id, blocks.sibling_index
  FROM blocks
  JOIN pages ON pages.id = blocks.page_id
 WHERE pages.name_key = ?1 AND blocks.preorder = ?2";

/// The neighbouring block on one side of a sibling index, or no row at the page's edge.
///
/// `parent_id IS ?2` is the predicate a plain `=` cannot write: a top-level block's parent is NULL,
/// and `parent_id = NULL` is never true in SQL, so an equality would report every top-level block as
/// having no siblings instead of the truth that two top-level blocks are siblings of each other.
/// `IS` compares against a bound NULL exactly as an equality compares against a value.
///
/// The page equality lets the `(page_id, preorder)` key narrow the candidates to the page's own rows
/// before the parent and sibling-index predicates apply; no index leads with `parent_id`, and one
/// page's blocks are the whole scan. The row shape is [`PAGE_BLOCK_VIEWS`]'s, so one assembler
/// serves both reads.
const PREVIOUS_SIBLING: &str = "\
SELECT blocks.preorder, blocks.depth, parent.preorder, blocks.text, blocks.durable_id,
       blocks.ephemeral_id, blocks.marker_kind, blocks.heading_level
  FROM blocks
  LEFT JOIN blocks AS parent ON parent.id = blocks.parent_id
 WHERE blocks.page_id = ?1
   AND blocks.parent_id IS ?2
   AND blocks.sibling_index < ?3
 ORDER BY blocks.sibling_index DESC
 LIMIT 1";

/// The next sibling, mirroring [`PREVIOUS_SIBLING`] on the other side of the index.
const NEXT_SIBLING: &str = "\
SELECT blocks.preorder, blocks.depth, parent.preorder, blocks.text, blocks.durable_id,
       blocks.ephemeral_id, blocks.marker_kind, blocks.heading_level
  FROM blocks
  LEFT JOIN blocks AS parent ON parent.id = blocks.parent_id
 WHERE blocks.page_id = ?1
   AND blocks.parent_id IS ?2
   AND blocks.sibling_index > ?3
 ORDER BY blocks.sibling_index
 LIMIT 1";

/// The block that states one `id::` UUID, with where it sits.
///
/// The filter is an equality on `durable_id`, served by `blocks_by_durable_id`; the page follows by
/// primary key, and the parent block follows by its own primary key, so adding the parent's position
/// to the row costs one key lookup rather than another read. The id is bound as the canonical
/// lowercase spelling [`BlockId::as_str`] returns, which is the only spelling the projection stores,
/// so no fold enters the comparison and the seek stays an exact one.
///
/// The rows are not capped. One statement has to answer "nothing, one, or more than one", and reading
/// the cursor to the end answers it: a unique id stops at its one row, and a duplicated one pays for
/// its duplicates. A `LIMIT 2` would also answer that question, but it would truncate the pages the
/// error message has to name, and learning that there are more would take the second statement this
/// lookup exists to avoid.
pub(super) const BLOCK_BY_DURABLE_ID: &str = "\
SELECT pages.name,
       blocks.preorder,
       blocks.parent_id,
       parent.preorder,
       blocks.depth,
       blocks.text,
       blocks.durable_id,
       blocks.marker_kind,
       blocks.heading_level
  FROM blocks
  JOIN pages ON pages.id = blocks.page_id
  LEFT JOIN blocks AS parent ON parent.id = blocks.parent_id
 WHERE blocks.durable_id = ?1";

/// What the rows say about `id`: nothing, one block stating it, or more than one.
///
/// The stored value is read back through [`BlockId::parse`], so a row that got in with something
/// the shape rule no longer accepts surfaces as [`IndexError::CorruptRow`] instead of being handed
/// to a caller as identity. Duplicates are not prevented anywhere in this layer: if two rows hold
/// one id, this read says so instead of answering one of them. Which page a person should look at is
/// not a question the rows can settle, so the answer names every page involved and lets the caller
/// decide.
pub(in crate::index) fn block_by_durable_id(
    conn: &Connection,
    id: &BlockId,
) -> Result<DurableLookup, IndexError> {
    let mut statement = conn.prepare_cached(BLOCK_BY_DURABLE_ID)?;
    let mut rows = statement.query([id.as_str()])?;
    let mut located = Vec::new();
    while let Some(row) = rows.next()? {
        let page = row.get::<_, String>(0)?;
        let stored = row.get::<_, String>(6)?;
        let durable_id = BlockId::parse(&stored).map_err(|_| IndexError::CorruptRow {
            column: "blocks.durable_id",
            value: stored,
        })?;
        located.push(LocatedBlock {
            page,
            preorder: row.get::<_, u32>(1)?,
            parent_id: row.get::<_, Option<i64>>(2)?,
            parent_preorder: row.get::<_, Option<u32>>(3)?,
            depth: row.get::<_, u32>(4)?,
            text: row.get::<_, String>(5)?,
            marker: decode_marker(&row.get::<_, String>(7)?, row.get::<_, Option<u32>>(8)?)?,
            durable_id,
        });
    }

    // One statement answers all three cases: the cursor count is what "nothing, one, or more than
    // one" means, so no second query is spent on counting rows the first one already read.
    Ok(match located.len() {
        0 => DurableLookup::Absent,
        1 => DurableLookup::One(located.remove(0)),
        _ => {
            let mut pages: Vec<String> = located.into_iter().map(|block| block.page).collect();
            // Sorted and deduplicated so the answer is a product of the rows, not of their order, and
            // so a person reads a page once however many blocks on it state the id.
            pages.sort_unstable();
            pages.dedup();
            DurableLookup::Several { pages }
        }
    })
}

/// The block `path` names in the page `page`, or `None` when nothing answers.
///
/// `path[0]` is the **preorder** of the top-level block the path starts at, and every later element
/// is the position of that level's block among its siblings (`blocks.sibling_index`). This is the
/// address a document edit answers with after it inserts a block, so the write path can read the row
/// it just created back out of the projection. The descent reads the page's block skeleton once,
/// through the same [`page_blocks`] a page-tree read uses, and walks it in memory: one level per
/// element, one block per level. A level with nothing at that position, or a first element that names
/// no top-level block, is a miss.
///
/// The skeleton carries no text, so the block's own text and its identity are read afterwards by the
/// `(page_id, preorder)` key that `blocks` is unique on. Both statements read in one transaction, so
/// the position and the content describe one revision.
///
/// The page is resolved by [`page_name_key`], so any spelling of the page's name answers for it, and
/// the returned page name is the stored spelling.
pub(in crate::index) fn block_by_page_path(
    conn: &Connection,
    page: &str,
    path: &[u32],
) -> Result<Option<LocatedPageBlock>, IndexError> {
    // An empty path names no block. That is a structural miss, not a corrupt request: the caller's
    // address simply resolves to nothing.
    let Some((root_preorder, deeper)) = path.split_first() else {
        return Ok(None);
    };

    let tx = super::super::ReadTransaction::new(conn)?;
    let Some((Some(page_id), metadata)) = page_row(&tx, page)? else {
        return Ok(None);
    };
    let blocks = page_blocks(&tx, page_id)?;

    // The first element names a top-level block by preorder, so only roots are candidates: a nested
    // block's preorder must not answer as if it were a root.
    let Some(mut found) = blocks.iter().find(|node| node.preorder() == *root_preorder) else {
        return Ok(None);
    };
    let mut parent_preorder = None;
    for &sibling in deeper {
        let Some(node) = found
            .children()
            .iter()
            .find(|node| node.sibling_index() == sibling)
        else {
            return Ok(None);
        };
        parent_preorder = Some(found.preorder());
        found = node;
    }

    let located = block_row_at(
        &tx,
        page_id,
        found.preorder(),
        found.depth(),
        parent_preorder,
    )?;
    let Some(mut located) = located else {
        // The skeleton and the block row came from one transaction, so the row the tree was built
        // from must still be there. Its absence is inconsistent derived state, not a miss.
        return Err(IndexError::CorruptRow {
            column: "blocks.preorder",
            value: found.preorder().to_string(),
        });
    };
    located.page = metadata.name().to_owned();
    tx.commit()?;
    Ok(Some(located))
}

/// The block at `preorder` in the page called `page`, or `None` when the page has no such block.
///
/// This is the answer a write reads after it edited one block of a page, addressed by the position
/// the block held before the edit: an edit that replaces a block's text or adds a property line keeps
/// its preorder, while every ephemeral id of the page is replaced by the re-projection that follows
/// (ADR-0012). Reading the row back by position is therefore the only way to answer with the id the
/// new rows hold.
///
/// The page is resolved by [`page_name_key`], so any spelling of the page's name answers for it, and
/// the returned page name is the stored spelling.
pub(in crate::index) fn block_by_page_preorder(
    conn: &Connection,
    page: &str,
    preorder: u32,
) -> Result<Option<LocatedPageBlock>, IndexError> {
    let tx = super::super::ReadTransaction::new(conn)?;
    let Some((Some(page_id), metadata)) = page_row(&tx, page)? else {
        return Ok(None);
    };
    let row = tx
        .query_row(
            "SELECT blocks.depth, parent.preorder
               FROM blocks
               LEFT JOIN blocks AS parent ON parent.id = blocks.parent_id
              WHERE blocks.page_id = ?1 AND blocks.preorder = ?2",
            params![page_id, preorder],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, Option<u32>>(1)?)),
        )
        .optional()?;
    let Some((depth, parent_preorder)) = row else {
        return Ok(None);
    };
    let located = block_row_at(&tx, page_id, preorder, depth, parent_preorder)?;
    tx.commit()?;
    Ok(located.map(|mut located| {
        located.page = metadata.name().to_owned();
        located
    }))
}

/// Read one block row of `page_id` by its preorder, with the identity columns it states.
///
/// The caller has already established that the block exists and supplies the two position facts it
/// read to find it, so this is one seek on the `(page_id, preorder)` key. The page name is left empty
/// for the caller to fill from the page row it resolved.
fn block_row_at(
    conn: &Connection,
    page_id: i64,
    preorder: u32,
    depth: u32,
    parent_preorder: Option<u32>,
) -> Result<Option<LocatedPageBlock>, IndexError> {
    let row = conn
        .query_row(
            "SELECT text, durable_id, ephemeral_id, marker_kind, heading_level FROM blocks
              WHERE page_id = ?1 AND preorder = ?2",
            params![page_id, preorder],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((text, durable_id, ephemeral_id, kind, level)) = row else {
        return Ok(None);
    };
    Ok(Some(LocatedPageBlock {
        page: String::new(),
        preorder,
        parent_preorder,
        depth,
        text,
        marker: decode_marker(&kind, level)?,
        selector: row_selector(durable_id, ephemeral_id)?,
    }))
}

/// The block one ephemeral id names, with where it sits and the fingerprint of the page it was
/// projected from.
///
/// This is the lookup that makes an ephemeral id fail closed (ADR-0012). The filter is an equality on
/// `blocks.ephemeral_id`, served by `blocks_by_ephemeral_id`, so the answer is one stored row or no
/// row: there is no position to re-derive and nothing to search for. A page name follows by primary
/// key, the parent's position by its own key, and the page's content fingerprint by the
/// `pages.relative_path` foreign key into `file_baseline` — the fingerprint of the bytes those rows
/// were projected from, which is what a write compares the file against before changing it.
const BLOCK_BY_EPHEMERAL_ID: &str = "\
SELECT blocks.id,
       pages.name,
       blocks.preorder,
       blocks.depth,
       parent.preorder,
       blocks.text,
       file_baseline.content_hash,
       blocks.marker_kind,
       blocks.heading_level
  FROM blocks
  JOIN pages ON pages.id = blocks.page_id
  JOIN file_baseline ON file_baseline.relative_path = pages.relative_path
  LEFT JOIN blocks AS parent ON parent.id = blocks.parent_id
 WHERE blocks.ephemeral_id = ?1";

/// The child-index path of one block: its position among the page's top-level blocks, then the
/// position of each deeper block among its siblings.
///
/// This is the address a document edit resolves (`BlockLocator::Indices`) and the same path
/// `SnapshotBlock::child_path` reports, so a write can hand it straight to the document layer
/// without translating one spelling into another. At most one row per level, walked up by row key:
/// `MAX_BLOCK_DEPTH` bounds the row count.
const BLOCK_CHILD_PATH: &str = "\
WITH RECURSIVE chain(id, parent_id, sibling_index, level) AS (
    SELECT id, parent_id, sibling_index, 0 FROM blocks WHERE id = ?1
    UNION ALL
    SELECT parent.id, parent.parent_id, parent.sibling_index, chain.level + 1
      FROM blocks AS parent
      JOIN chain ON parent.id = chain.parent_id
)
SELECT sibling_index FROM chain ORDER BY level DESC";

/// The block `id` names, or `None` when no row holds that id.
///
/// A miss is the only way a stale ephemeral id can answer, and that is deliberate: an id survives
/// exactly as long as its row, and a page's rows are replaced wholesale when its bytes change
/// (ADR-0012). Nothing here looks for a block that "must have been" this one.
pub(in crate::index) fn block_by_ephemeral_id(
    conn: &Connection,
    id: &EphemeralId,
) -> Result<Option<LocatedEphemeralBlock>, IndexError> {
    let tx = super::super::ReadTransaction::new(conn)?;
    let row = tx
        .query_row(BLOCK_BY_EPHEMERAL_ID, [id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, Option<u32>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<u32>>(8)?,
            ))
        })
        .optional()?;
    let Some((row_id, page, preorder, depth, parent_preorder, text, content_hash, kind, level)) =
        row
    else {
        return Ok(None);
    };

    // The path is read second because it depends on the row key the first statement found, and it
    // walks up the parent chain rather than down the page: the row is one seek, and each level above
    // it is another key lookup, never a read of the page's other blocks.
    let mut child_path = Vec::new();
    {
        let mut statement = tx.prepare(BLOCK_CHILD_PATH)?;
        let mut rows = statement.query([row_id])?;
        while let Some(step) = rows.next()? {
            child_path.push(step.get::<_, u32>(0)?);
        }
    }
    if child_path.is_empty() {
        // The chain starts at the row the first statement found, so it can never be empty while that
        // row exists; an empty one means the two statements disagree about the same transaction.
        return Err(IndexError::CorruptRow {
            column: "blocks.parent_id",
            value: format!("no child path for block row {row_id}"),
        });
    }
    tx.commit()?;

    Ok(Some(LocatedEphemeralBlock {
        page,
        preorder,
        depth,
        parent_preorder,
        text,
        marker: decode_marker(&kind, level)?,
        child_path,
        page_fingerprint: content_hash as u64,
    }))
}

/// Property lines of one block, in written order, or `None` when the page has no derived rows.
///
/// The block is addressed by the `(page, preorder)` pair of a view the engine has already located by
/// public identity, so no selector reaches this layer. A page that does not exist answers `None` —
/// the same "absent page" answer every page read gives — while a page without a block at that
/// position is corrupt derived state: the caller located the block in the same index this read runs
/// against, so the row it saw cannot be gone.
///
/// The comparison is on [`page_name_key`], so the caller may use any spelling of the page's name.
/// A block without property lines answers with an empty list.
pub(in crate::index) fn block_properties(
    conn: &Connection,
    page: &str,
    preorder: u32,
) -> Result<Option<Vec<(String, String)>>, IndexError> {
    let Some(row) = conn
        .query_row(
            "SELECT id FROM pages WHERE name_key = ?1",
            [page_name_key(page)],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let Some(row) = conn
        .query_row(
            "SELECT id FROM blocks WHERE page_id = ?1 AND preorder = ?2",
            params![row, preorder],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Err(IndexError::CorruptRow {
            column: "blocks.preorder",
            value: preorder.to_string(),
        });
    };
    let mut stmt = conn.prepare(BLOCK_PROPERTIES_BY_ID)?;
    let mut rows = stmt.query([row])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push((row.get(0)?, row.get(1)?));
    }
    Ok(Some(out))
}

/// Property lines of one located block, in written order.
///
/// `block_id` is the engine-resolved key, and the `(block_id, ordinal)` primary key answers the
/// filter and the order with one seek: the rows come back ordered without a sort.
const BLOCK_PROPERTIES_BY_ID: &str = "\
SELECT key, value
  FROM block_properties
 WHERE block_id = ?1
 ORDER BY ordinal";

/// The siblings on both sides of one block, or `None` when the page has no derived rows.
///
/// The three statements share one transaction, so the context row and the two neighbours describe
/// one revision even if a concurrent writer commits in between.
pub(in crate::index) fn sibling_views(
    conn: &Connection,
    page: &str,
    preorder: u32,
) -> Result<Option<BlockSiblings>, IndexError> {
    let tx = super::super::ReadTransaction::new(conn)?;
    let Some((page_id, name, parent_id, sibling_index)) = tx
        .query_row(
            SIBLING_CONTEXT,
            params![page_name_key(page), preorder],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let previous = sibling_view(
        &tx,
        PREVIOUS_SIBLING,
        page_id,
        parent_id,
        sibling_index,
        &name,
    )?;
    let next = sibling_view(&tx, NEXT_SIBLING, page_id, parent_id, sibling_index, &name)?;
    tx.commit()?;
    Ok(Some((previous, next)))
}

/// Read one neighbour through one of the sibling statements and assemble its view.
///
/// The row shape is [`PAGE_BLOCK_VIEWS`]'s, so the selector rule is the same one that read uses: the
/// durable id for a block that states an `id::`, the minted ephemeral id for one that does not.
fn sibling_view(
    conn: &Connection,
    sql: &str,
    page_id: i64,
    parent_id: Option<i64>,
    sibling_index: u32,
    page: &str,
) -> Result<Option<BlockView>, IndexError> {
    let row = conn
        .query_row(sql, params![page_id, parent_id, sibling_index], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<u32>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<u32>>(7)?,
            ))
        })
        .optional()?;
    let Some((preorder, depth, parent_preorder, text, durable_id, ephemeral_id, kind, level)) = row
    else {
        return Ok(None);
    };
    Ok(Some(BlockView::new(
        page.to_owned(),
        preorder,
        depth,
        parent_preorder,
        text,
        decode_marker(&kind, level)?,
        row_selector(durable_id, ephemeral_id)?,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::query_plan;

    use super::super::namespace::{NAMESPACE_CHILDREN, NAMESPACE_TREE};

    #[test]
    fn the_durable_id_lookup_seeks_its_index_instead_of_scanning_blocks() {
        // A durable-target read is one id, on a table that grows with the whole graph, so it must be
        // an index seek. The equality is written plainly on `durable_id`, so this also pins the
        // lookup to the stored spelling: a query that needed a fold to match would mean the next
        // reader could silently lose the index.
        let plan = query_plan(BLOCK_BY_DURABLE_ID);
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH blocks USING INDEX blocks_by_durable_id")),
            "the block must be found through blocks_by_durable_id: {plan:?}"
        );
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH pages USING INTEGER PRIMARY KEY")),
            "each hit must reach its page by primary key: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("SCAN")),
            "no step of a durable-id lookup may scan: {plan:?}"
        );
    }

    #[test]
    fn the_block_and_namespace_reads_seek_their_keys_instead_of_scanning() {
        // Block properties are addressed by one located block and a namespace by one folded key, so
        // each statement must be a seek: a plan that scans blocks or pages here means the
        // `(page_id, preorder)` or `name_key` predicate no longer has an index to land on.
        for (label, sql) in [
            ("block properties", BLOCK_PROPERTIES_BY_ID),
            ("namespace children", NAMESPACE_CHILDREN),
            ("namespace tree", NAMESPACE_TREE),
            ("sibling context", SIBLING_CONTEXT),
        ] {
            let plan = query_plan(sql);
            assert!(
                plan.iter().any(|step| step.contains("SEARCH")),
                "{label} must be answered by a seek: {plan:?}"
            );
            assert!(
                !plan
                    .iter()
                    .any(|step| step.contains("SCAN") && step != "SCAN page_entities"),
                "{label} must not scan a table or an index: {plan:?}"
            );
        }

        // The neighbour statements narrow to one page through the `(page_id, preorder)` key and
        // cannot seek the `parent_id IS ?` predicate, so the plan may hold one bounded page scan but
        // no unbounded one. The LIMIT keeps a huge page from paying for rows beyond the neighbour.
        for (label, sql) in [
            ("previous sibling", PREVIOUS_SIBLING),
            ("next sibling", NEXT_SIBLING),
        ] {
            let plan = query_plan(sql);
            assert!(
                plan.iter().any(|step| step.contains("SEARCH blocks")),
                "{label} must narrow blocks by the page key: {plan:?}"
            );
            assert!(
                !plan.iter().any(|step| step.contains("SCAN pages")),
                "{label} must not scan pages: {plan:?}"
            );
        }
    }
}
