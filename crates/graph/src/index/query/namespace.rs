use crate::domain::page_name_key;
use crate::index::IndexError;
use rusqlite::Connection;

/// Pages whose name sits inside one namespace, ordered by folded name.
///
/// The range is the half-open interval `[key || '/', key || '0')`, and `'0'` is the byte after
/// `'/'`. A key inside the interval either continues the namespace's own slash or falls between the
/// two bounds at the byte after `key`, and no stored name can: a name never ends in an empty
/// segment, and any other continuation byte sorts at or after `'0'`. The range therefore holds
/// exactly the keys that begin with `key || '/'`, which a `LIKE` prefix would need escaping to say
/// and an `substr` equality could not serve from the `name_key` index. The comparison is BINARY byte
/// order, the collation `pages.name_key` is stored under.
pub(super) const NAMESPACE_TREE: &str = "\
SELECT name
  FROM page_entities
 WHERE name_key >= ?1 || '/'
   AND name_key < ?1 || '0'
 ORDER BY name_key";

/// Direct children of one namespace: [`NAMESPACE_TREE`] plus "no second separator".
///
/// Every page below the namespace passes the range; a page one level down is the one whose name,
/// after the namespace's own slash, holds no further `/`. The substring starts at `length(?1) + 2`
/// because `substr` counts from one and the slash itself is the character at `length(?1) + 1`.
pub(super) const NAMESPACE_CHILDREN: &str = "\
SELECT name
  FROM page_entities
 WHERE name_key >= ?1 || '/'
   AND name_key < ?1 || '0'
   AND instr(substr(name_key, length(?1) + 2), '/') = 0
 ORDER BY name_key";

/// Pages whose name sits inside one namespace, in [`NAMESPACE_TREE`]'s interval, ordered by folded
/// name. `direct` keeps only the pages whose remainder holds no second separator.
///
/// Both forms answer an empty namespace the same way: an empty key falls inside every name that
/// starts with `/`, and no page does, so the interval matches nothing. A namespace no page extends
/// matches nothing too, which is the same "an empty answer, not an error" the other name reads give
/// for a name nothing points at.
fn namespace_pages(
    conn: &Connection,
    namespace: &str,
    direct: bool,
) -> Result<Vec<String>, IndexError> {
    let sql = if direct {
        NAMESPACE_CHILDREN
    } else {
        NAMESPACE_TREE
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([page_name_key(namespace)])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row.get(0)?);
    }
    Ok(out)
}

/// Pages that are direct children of one namespace, ordered by folded page name.
///
/// The namespace is matched through [`page_name_key`], so `A` and `a` ask for the same subtree, and
/// every returned name is the stored spelling. Only one level is reported: `a/b` is a child of `a`,
/// `a/b/c` is not.
pub(in crate::index) fn pages_from_namespace(
    conn: &Connection,
    namespace: &str,
) -> Result<Vec<String>, IndexError> {
    namespace_pages(conn, namespace, true)
}

/// Every page under one namespace, the whole subtree, ordered by folded page name.
pub(in crate::index) fn pages_tree_from_namespace(
    conn: &Connection,
    namespace: &str,
) -> Result<Vec<String>, IndexError> {
    namespace_pages(conn, namespace, false)
}
