use super::page::page_row;
use crate::domain::{page_name_key, Backlink, PageProperty, TagUsage};
use crate::index::IndexError;
use rusqlite::Connection;

/// Properties of one page in written order.
///
/// `ordinal` is the page's own sequence, so the primary key `(page_id, ordinal)` answers both the
/// filter and the order with one seek: the rows come back ordered without a sort.
const PAGE_PROPERTIES: &str = "\
SELECT key, normalized_key, value
  FROM page_properties
 WHERE page_id = ?1
 ORDER BY ordinal";

/// Pages that reference one target key, each page once, with the block that represents it.
///
/// `target_key` is [`page_name_key`] bound by the caller, so no spelling enters the comparison. The
/// filter is served by `page_refs_by_target`; `blocks` and `pages` are then reached by primary key.
/// Grouping by `pages.id` collapses several references from one page, and the order is the pages'
/// folded key, which is the same order [`pages_with_tag`] uses.
///
/// The group answers the page's **earliest** referencing block: `MIN(blocks.preorder)` is the only
/// aggregate, and SQLite then takes every bare column (`blocks.text` here) from an input row that
/// holds that minimum. That is a documented guarantee of a query with exactly one `min()` or
/// `max()`, so the text and the position always describe the same block.
const BACKLINKS: &str = "\
WITH RECURSIVE targets(key) AS (
    VALUES (?1)
    UNION
    SELECT claim.name_key
      FROM targets JOIN pages AS owner ON owner.name_key = targets.key
      JOIN page_claims AS claim ON claim.page_id = owner.id AND claim.kind = 'alias'
    UNION
    SELECT owner.name_key
      FROM targets JOIN page_claims AS claim ON claim.name_key = targets.key AND claim.kind = 'alias'
      JOIN pages AS owner ON owner.id = claim.page_id
), hits(page_id, preorder, text) AS (
    SELECT blocks.page_id, blocks.preorder, blocks.text
      FROM targets CROSS JOIN page_refs ON page_refs.target_key = targets.key
      JOIN blocks ON blocks.id = page_refs.block_id
    UNION ALL
    SELECT blocks.page_id, blocks.preorder, blocks.text
      FROM targets CROSS JOIN tags ON tags.name_key = targets.key
      JOIN blocks ON blocks.id = tags.block_id
    UNION ALL
    SELECT claim.page_id, -1, (
        SELECT group_concat(key || ':: ' || value, char(10) ORDER BY ordinal)
          FROM page_properties WHERE page_properties.page_id = claim.page_id
    )
      FROM targets CROSS JOIN page_claims AS claim ON claim.name_key = targets.key
     WHERE claim.kind IN ('ref', 'tag')
)
SELECT source.name, MIN(hits.preorder), hits.text
  FROM hits CROSS JOIN pages AS source ON source.id = hits.page_id
 GROUP BY source.id
 ORDER BY source.name_key";

/// Every tag with the number of distinct pages that use it, ordered by folded key.
///
/// A tag has no target to seek, so this query necessarily enumerates the whole tag table: reading
/// every distinct tag is the question, not a lookup of one. The order is the index's own, so the walk
/// goes through `tags_by_name` and no sort is needed. `MIN(name)` picks one spelling of a tag
/// deterministically, and the count is over pages rather than blocks, so several blocks of one page
/// still count once.
///
/// The plan is `SCAN tags USING INDEX tags_by_name`, and no predicate can improve it: a filter on the
/// key that every row matches leaves the same rows to read, and the query selects `tags.name`, which
/// the index does not hold, so no form of this read is index-only. The test asserts the index walk
/// rather than a seek it cannot have.
const TAGS: &str = "\
WITH usages(name, name_key, page_id) AS (
    SELECT tags.name, tags.name_key, blocks.page_id
      FROM tags INDEXED BY tags_by_name JOIN blocks ON blocks.id = tags.block_id
    UNION ALL
    SELECT name, name_key, page_id FROM page_claims WHERE kind = 'tag'
)
SELECT MIN(name), name_key, COUNT(DISTINCT page_id)
  FROM usages GROUP BY name_key ORDER BY name_key";

/// Pages that use one tag key, each page once, ordered by folded page name.
///
/// The filter is a seek on `tags_by_name`; blocks and pages follow by primary key.
const PAGES_WITH_TAG: &str = "\
SELECT source.name FROM pages AS source
 WHERE source.id IN (
    SELECT blocks.page_id FROM tags JOIN blocks ON blocks.id = tags.block_id
     WHERE tags.name_key = ?1
    UNION
    SELECT page_id FROM page_claims WHERE name_key = ?1 AND kind = 'tag'
 )
 ORDER BY source.name_key";

/// Properties of one page in written order, or `None` when the page has no derived rows.
///
/// A page without property lines answers `Some` with an empty list: it exists and states no
/// properties, which is a different answer from a page that does not exist. Both statements read in
/// one transaction, so the existence check and the rows describe the same revision.
pub(in crate::index) fn page_properties(
    conn: &Connection,
    name: &str,
) -> Result<Option<Vec<PageProperty>>, IndexError> {
    let tx = super::super::ReadTransaction::new(conn)?;
    let Some((page_id, _)) = page_row(&tx, name)? else {
        return Ok(None);
    };
    let properties = {
        let mut stmt = tx.prepare(PAGE_PROPERTIES)?;
        let mut rows = stmt.query([page_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(PageProperty::new(row.get(0)?, row.get(1)?, row.get(2)?));
        }
        out
    };
    tx.commit()?;
    Ok(Some(properties))
}

/// Pages that reference `name`, matched by name key, each page once, ordered by page name key.
///
/// The target needs no stored page: `[[new page]]` is a reference to a page that may have no file
/// yet, so an unresolved name still answers with the pages that point at it.
pub(in crate::index) fn backlinks(
    conn: &Connection,
    name: &str,
) -> Result<Vec<Backlink>, IndexError> {
    let mut stmt = conn.prepare(BACKLINKS)?;
    let mut rows = stmt.query([page_name_key(name)])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let preorder: Option<u32> = row.get::<_, i64>(1)?.try_into().ok();
        out.push(Backlink::new(row.get(0)?, preorder, row.get(2)?));
    }
    Ok(out)
}

/// Every tag with the number of distinct pages that use it, ordered by folded tag name.
pub(in crate::index) fn tags(conn: &Connection) -> Result<Vec<TagUsage>, IndexError> {
    let mut stmt = conn.prepare(TAGS)?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(TagUsage::new(row.get(0)?, row.get(2)?));
    }
    Ok(out)
}

/// Pages that use `tag`, matched by name key, each page once, ordered by page name key.
pub(in crate::index) fn pages_with_tag(
    conn: &Connection,
    tag: &str,
) -> Result<Vec<String>, IndexError> {
    let mut stmt = conn.prepare(PAGES_WITH_TAG)?;
    let mut rows = stmt.query([page_name_key(tag)])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row.get(0)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::query_plan;

    use super::super::page::PAGE_ROW;

    #[test]
    fn every_keyed_fact_read_seeks_an_index_instead_of_scanning_the_table() {
        // A fact read is answered from one key. A plan that scans a table here means a missing index
        // or a predicate that no index can serve, and the read grows with the whole graph. `tags()`
        // is not in this list because it asks for every tag and therefore reads every row; its plan
        // is asserted separately.
        for (label, sql) in [
            ("page row", PAGE_ROW),
            ("page properties", PAGE_PROPERTIES),
            ("backlinks", BACKLINKS),
            ("pages with tag", PAGES_WITH_TAG),
        ] {
            let plan = query_plan(sql);
            assert!(
                plan.iter().any(|step| step.contains("SEARCH")),
                "{label} must be answered by a seek: {plan:?}"
            );
            assert!(
                !plan.iter().any(|step| step.starts_with("SCAN")
                    && !["SCAN hits", "SCAN targets", "SCAN CONSTANT ROW"]
                        .contains(&step.as_str())),
                "{label} must not scan a table or an index: {plan:?}"
            );
        }
    }

    #[test]
    fn the_tag_list_walks_its_key_index_instead_of_the_table() {
        // Listing every tag is an enumeration by definition, so the only thing a plan can promise is
        // that the walk follows `tags_by_name` in key order. No predicate can turn this into a seek:
        // every row matches, and the read needs `tags.name`, which the index does not hold.
        let plan = query_plan(TAGS);
        assert!(
            plan.iter()
                .any(|step| step.contains("SCAN tags USING INDEX tags_by_name")),
            "the tag list must walk tags_by_name, not the table: {plan:?}"
        );
        assert!(
            plan.iter()
                .filter(|step| step.starts_with("SCAN"))
                .all(|step| step.contains("SCAN tags USING INDEX tags_by_name")
                    || ["SCAN page_claims", "SCAN usages"].contains(&step.as_str())),
            "only tags, their page claims and the derived usages may be scanned: {plan:?}"
        );
    }

    #[test]
    fn a_page_and_the_key_that_points_at_it_are_seeked_not_scanned() {
        // A page read, a backlink read, and a tag-page read all start from one key. The page read is
        // the one that used to compare the caller's spelling with `pages.name`, which both missed
        // the page under another spelling and could not use the key index.
        let page = query_plan(PAGE_ROW);
        assert!(
            page.iter().any(|step| step.contains("SEARCH pages USING"))
                && page.iter().any(|step| step.contains("(name_key=?)")),
            "a page read must seek pages by its key: {page:?}"
        );

        let backlinks = query_plan(BACKLINKS);
        assert!(
            backlinks
                .iter()
                .any(|step| step.contains("page_refs_by_target")),
            "a backlink read must seek page_refs_by_target: {backlinks:?}"
        );

        let with_tag = query_plan(PAGES_WITH_TAG);
        assert!(
            with_tag.iter().any(|step| step.contains("tags_by_name")),
            "a tag-page read must seek tags_by_name: {with_tag:?}"
        );

        let tags = query_plan(TAGS);
        assert!(
            tags.iter()
                .any(|step| step.contains("tags USING INDEX tags_by_name")),
            "the tag list must walk tags_by_name instead of the table: {tags:?}"
        );
    }
}
