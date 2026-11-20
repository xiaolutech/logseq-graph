use super::{QueryExecError, Set, Sub};
use crate::document::{extract_page_refs, extract_tags, normalized_property_key};
use crate::domain::{
    list_value_matches, page_name_key, search_key, DateOperand, JournalDate, QueryContext,
    QueryError, QueryExpr,
};
#[cfg(test)]
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};

/// One predicate, as a set of row keys of `set`.
pub(super) fn leaf(
    conn: &Connection,
    expr: &QueryExpr,
    set: Set,
    ctx: &QueryContext,
) -> Result<Sub, QueryExecError> {
    match expr {
        QueryExpr::Content(text) => {
            // Content search and the search read use the same fold for the same reason: an ASCII
            // needle keeps the built-in `lower` statement (its answer is identical), any other
            // needle needs the stored text folded through `search_key` too. The needle is folded
            // once here, not once per row.
            let (sql, needle) = if text.is_ascii() {
                (
                    "SELECT id FROM blocks WHERE instr(lower(text), lower(?)) > 0",
                    text.clone(),
                )
            } else {
                (
                    "SELECT id FROM blocks WHERE instr(search_key(text), ?) > 0",
                    search_key(text),
                )
            };
            Ok(Sub::new(
                match set {
                    Set::Blocks => sql.to_owned(),
                    // A page's own text is its properties and its blocks; a page-level expression that
                    // is a bare word cannot arise (`is_page_level` says a bare word is a block query),
                    // so this arm is unreachable in practice and answers nothing rather than guessing.
                    Set::Pages => "SELECT id FROM pages WHERE 0".to_owned(),
                },
                vec![Value::Text(needle)],
            ))
        }
        QueryExpr::Journal => Ok(Sub::new(
            match set {
                Set::Blocks => {
                    "SELECT blocks.id FROM blocks JOIN pages ON pages.id = blocks.page_id \
                                WHERE pages.kind = 'journal'"
                        .to_owned()
                }
                Set::Pages => "SELECT id FROM pages WHERE kind = 'journal'".to_owned(),
            },
            Vec::new(),
        )),
        QueryExpr::Page(name) => Ok(Sub::new(
            match set {
                Set::Blocks => {
                    "SELECT blocks.id FROM blocks JOIN pages ON pages.id = blocks.page_id \
                                WHERE pages.name_key = ?"
                        .to_owned()
                }
                Set::Pages => "SELECT id FROM pages WHERE name_key = ?".to_owned(),
            },
            vec![Value::Text(page_name_key(name))],
        )),
        QueryExpr::Namespace(name) => {
            let key = page_name_key(name);
            Ok(Sub::new(
                match set {
                    Set::Blocks => "SELECT blocks.id FROM blocks \
                           JOIN pages ON pages.id = blocks.page_id \
                          WHERE pages.name_key = ? \
                             OR (pages.name_key >= ? || '/' AND pages.name_key < ? || '0')"
                        .to_owned(),
                    Set::Pages => "SELECT id FROM pages WHERE name_key = ? \
                                     OR (name_key >= ? || '/' AND name_key < ? || '0')"
                        .to_owned(),
                },
                match set {
                    Set::Blocks => vec![
                        Value::Text(key.clone()),
                        Value::Text(key.clone()),
                        Value::Text(key),
                    ],
                    Set::Pages => vec![
                        Value::Text(key.clone()),
                        Value::Text(key.clone()),
                        Value::Text(key),
                    ],
                },
            ))
        }
        QueryExpr::Task(markers) => {
            let placeholders = placeholders(markers.len());
            let sql = match set {
                Set::Blocks => {
                    format!("SELECT id FROM blocks WHERE task_marker IN ({placeholders})")
                }
                Set::Pages => "SELECT id FROM pages WHERE 0".to_owned(),
            };
            Ok(Sub::new(
                sql,
                markers
                    .iter()
                    .map(|marker| Value::Text(marker.as_str().to_owned()))
                    .collect(),
            ))
        }
        QueryExpr::Priority(priorities) => {
            let placeholders = placeholders(priorities.len());
            let sql = match set {
                Set::Blocks => format!("SELECT id FROM blocks WHERE priority IN ({placeholders})"),
                Set::Pages => "SELECT id FROM pages WHERE 0".to_owned(),
            };
            Ok(Sub::new(
                sql,
                priorities
                    .iter()
                    .map(|priority| Value::Text(priority.as_str().to_owned()))
                    .collect(),
            ))
        }
        QueryExpr::PageRef(name) => page_ref(conn, name, set),
        QueryExpr::Property { key, value } => property(conn, key, value.as_deref(), set),
        QueryExpr::PageProperty { key, value } => page_property(conn, key, value.as_deref(), set),
        QueryExpr::PageTags(tags) => page_tags(conn, tags, set),
        QueryExpr::Between(start, end) => between(conn, start, end, set, ctx),
        QueryExpr::And(_) | QueryExpr::Or(_) | QueryExpr::Not(_) => {
            unreachable!("a combination is handled by eval")
        }
    }
}

/// One anonymous placeholder per item.
///
/// Anonymous rather than numbered: a composed query concatenates several sub-selects, and a
/// numbered placeholder would make `?1` in the second sub-select rebind the first one's value. Bound
/// positionally, the order of the parameters is the order of the sub-selects, which is the order
/// this module builds them in.
fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `[[x]]`, `#x`, `(page-ref x)`: the page itself, the blocks that point at it, and what those
/// blocks carry with them.
///
/// Four things answer, because all four are what Logseq's simple query returns for a reference:
///
/// * the blocks of the page itself — asking for `[[x]]` on the page `x` answers its own blocks;
/// * the blocks that reference `x` through `[[x]]` or `#x`, **and every block under them**, because
///   Logseq's path references are inherited by a block's descendants;
/// * the blocks of a page whose properties name `x`, which is how `tags:: [[x]]` makes a page part
///   of the answer;
/// * nothing else. A reference from a block's own property value is that block's reference, so it
///   is already the middle case.
fn page_ref(conn: &Connection, name: &str, set: Set) -> Result<Sub, QueryExecError> {
    let _ = conn;
    let key = page_name_key(name);
    match set {
        Set::Pages => Ok(Sub::new(
            "SELECT id FROM pages WHERE name_key = ?",
            vec![Value::Text(key)],
        )),
        Set::Blocks => {
            // The seeds are direct hits; the recursive part adds every descendant of a seed. `UNION`
            // deduplicates, so a seed that sits under another seed contributes once.
            // `WITH` has to open the whole compound select, so the seeds come first and the page's
            // own blocks are the second arm.
            let sql = "\
WITH RECURSIVE seed(id) AS (
        SELECT block_id FROM page_refs WHERE target_key = ?
        UNION
        SELECT block_id FROM tags WHERE name_key = ?
        UNION
        SELECT blocks.id FROM page_claims JOIN blocks USING (page_id)
         WHERE page_claims.name_key = ? AND page_claims.kind IN ('ref', 'tag')
     ),
     under(id) AS (
        SELECT id FROM seed
        UNION
        SELECT blocks.id FROM blocks JOIN under ON blocks.parent_id = under.id
     )
SELECT id FROM under
UNION
SELECT blocks.id
  FROM blocks
  JOIN pages ON pages.id = blocks.page_id
 WHERE pages.name_key = ?";
            Ok(Sub::new(
                sql,
                vec![
                    Value::Text(key.clone()),
                    Value::Text(key.clone()),
                    Value::Text(key.clone()),
                    Value::Text(key),
                ],
            ))
        }
    }
}

/// An indexed property lookup; value interpretation runs inside SQLite, without collecting owners.
fn owners_matching(
    _conn: &Connection,
    table: &str,
    owner: &str,
    key: &str,
    wanted: Option<&str>,
) -> Result<Sub, QueryExecError> {
    let normalized = normalized_property_key(key);
    // The last line under a key wins, which is the rule every other property read applies.
    let sql = format!(
        "SELECT {owner} AS id FROM {table} \
          WHERE normalized_key = ? \
            AND ordinal = (SELECT MAX(ordinal) FROM {table} AS later \
                            WHERE later.normalized_key = {table}.normalized_key \
                              AND later.{owner} = {table}.{owner}) \
            AND query_property_matches(value, normalized_key, ?)"
    );
    Ok(Sub::new(
        sql,
        vec![
            Value::Text(normalized),
            wanted.map_or(Value::Null, |v| Value::Text(v.to_owned())),
        ],
    ))
}

pub(crate) fn register_functions(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "query_property_matches",
        3,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let value = ctx.get::<String>(0)?;
            let key = ctx.get::<String>(1)?;
            let wanted = ctx.get::<Option<String>>(2)?;
            Ok(property_value_matches(&value, &key, wanted.as_deref()))
        },
    )
}

/// Whether one raw property value denotes `wanted`, per the branch's rules.
///
/// Three ways, in the order that matches what a person writes:
///
/// * the value holds a reference to `wanted` (`[[wanted]]`, `[[wanted|alias]]`, `#wanted`), compared
///   under the page-name key, because a property value that links names a page;
/// * the value is one of the deliberately list-valued keys (`tags`, `alias`, `topics`) and one
///   element of the comma-separated list equals `wanted`;
/// * the value is the scalar `wanted` as written, quoted or not.
///
/// The custom comma-separated key list a person can configure is not read here: it is configuration
/// this build does not load yet, and inventing a default list of it would be guessing. Those values
/// are still matched by reference and by exact text.
fn property_value_matches(value: &str, normalized_key: &str, wanted: Option<&str>) -> bool {
    let Some(wanted) = wanted else {
        return true;
    };
    let wanted = wanted.trim().trim_start_matches('#').trim();
    let wanted = wanted
        .strip_prefix("[[")
        .and_then(|rest| rest.strip_suffix("]]"))
        .unwrap_or(wanted);
    let wanted_key = page_name_key(wanted);

    for reference in extract_page_refs(value) {
        if page_name_key(&reference) == wanted_key {
            return true;
        }
    }
    for tag in extract_tags(value) {
        if page_name_key(&tag) == wanted_key {
            return true;
        }
    }
    if matches!(normalized_key, "tags" | "alias" | "topics") && list_value_matches(value, wanted) {
        return true;
    }
    let scalar = value.trim().trim_matches('"').trim();
    scalar == wanted || page_name_key(scalar) == wanted_key
}

fn property(
    conn: &Connection,
    key: &str,
    value: Option<&str>,
    set: Set,
) -> Result<Sub, QueryExecError> {
    let ids = owners_matching(conn, "block_properties", "block_id", key, value)?;
    match set {
        Set::Blocks => Ok(ids),
        // A block property is a question about blocks; a page-level expression cannot contain one
        // (`is_page_level` says so), so this arm answers nothing rather than promoting silently.
        Set::Pages => Ok(Sub::new("SELECT id FROM pages WHERE 0", Vec::new())),
    }
}

fn page_property(
    conn: &Connection,
    key: &str,
    value: Option<&str>,
    set: Set,
) -> Result<Sub, QueryExecError> {
    let ids = owners_matching(conn, "page_properties", "page_id", key, value)?;
    match set {
        Set::Pages => Ok(ids),
        Set::Blocks => Ok(Sub::new(
            format!("SELECT id FROM blocks WHERE page_id IN ({})", ids.sql),
            ids.params,
        )),
    }
}

fn page_tags(conn: &Connection, tags: &[String], set: Set) -> Result<Sub, QueryExecError> {
    // Every tag must be present, so the pages are intersected one tag at a time.
    let mut parts = Vec::new();
    let mut params = Vec::new();
    for tag in tags {
        let ids = owners_matching(conn, "page_properties", "page_id", "tags", Some(tag))?;
        parts.push(ids.sql);
        params.extend(ids.params);
    }
    let pages = parts.join(" INTERSECT ");
    match set {
        Set::Pages => Ok(Sub::new(pages, params)),
        Set::Blocks => Ok(Sub::new(
            format!("SELECT id FROM blocks WHERE page_id IN ({pages})"),
            params,
        )),
    }
}

fn between(
    conn: &Connection,
    start: &DateOperand,
    end: &DateOperand,
    set: Set,
    ctx: &QueryContext,
) -> Result<Sub, QueryExecError> {
    let low = resolve_day(conn, start, ctx)?;
    let high = resolve_day(conn, end, ctx)?;
    let (low, high) = if low <= high {
        (low.to_string(), high.to_string())
    } else {
        (high.to_string(), low.to_string())
    };
    Ok(Sub::new(
        match set {
            Set::Blocks => "SELECT blocks.id FROM blocks JOIN pages ON pages.id = blocks.page_id \
                            WHERE pages.kind = 'journal' AND pages.journal_date BETWEEN ? AND ?"
                .to_owned(),
            Set::Pages => "SELECT id FROM pages WHERE kind = 'journal' \
                            AND journal_date BETWEEN ? AND ?"
                .to_owned(),
        },
        vec![Value::Text(low), Value::Text(high)],
    ))
}

/// One `(between …)` operand as a calendar day.
fn resolve_day(
    conn: &Connection,
    operand: &DateOperand,
    ctx: &QueryContext,
) -> Result<JournalDate, QueryExecError> {
    let overflow = |operand: String| QueryError::UnresolvedDate {
        operand,
        detail: "the offset does not name a day on the calendar".to_owned(),
    };
    match operand {
        DateOperand::Day(day) => Ok(*day),
        DateOperand::Today => Ok(ctx.today()),
        DateOperand::OffsetDays(days) => Ok(ctx
            .today()
            .checked_add_days(*days)
            .ok_or_else(|| overflow(format!("{days:+}d")))?),
        DateOperand::OffsetMonths(months) => Ok(ctx
            .today()
            .checked_add_months(*months)
            .ok_or_else(|| overflow(format!("{months:+}m")))?),
        DateOperand::JournalPage(name) => {
            let key = page_name_key(name);
            let stored: Option<Option<String>> = conn
                .query_row(
                    "SELECT journal_date FROM pages WHERE name_key = ? AND kind = 'journal'",
                    [&key],
                    |row| row.get(0),
                )
                .optional()?;
            match stored {
                Some(Some(day)) => Ok(JournalDate::parse_stored(&day)
                    .ok_or_else(|| overflow(format!("[[{name}]]")))?),
                Some(None) | None => Err(QueryError::UnresolvedDate {
                    operand: format!("[[{name}]]"),
                    detail: "no journal page of this graph has that name".to_owned(),
                }
                .into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(conn: &Connection, sql: &str, params: &[Value]) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("prepare plan");
        let rows = stmt
            .query_map(params_from_iter(params.iter()), |row| {
                row.get::<_, String>(3)
            })
            .expect("explain")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read plan");
        rows
    }

    #[test]
    fn a_property_leaf_seeks_the_key_index() {
        let conn = Connection::open_in_memory().expect("open");
        crate::index::schema::init(&conn).expect("schema");
        let sql = "SELECT block_id, value FROM block_properties \
                    WHERE normalized_key = ? \
                      AND ordinal = (SELECT MAX(ordinal) FROM block_properties AS later \
                                      WHERE later.normalized_key = ? \
                                        AND later.block_id = block_properties.block_id)";
        let steps = plan(
            &conn,
            sql,
            &[Value::Text("k".into()), Value::Text("k".into())],
        );
        assert!(
            steps
                .iter()
                .any(|step| step.contains("block_properties_by_name")),
            "the key lookup must seek its index: {steps:?}"
        );
    }

    /// A plan that scans `blocks` is the failure mode the partial indexes can create: the leaf is
    /// an `IN` list, and SQLite only uses a partial index when the WHERE clause implies the index's
    /// predicate (`IS NOT NULL` here). If that implication ever stops being proved, the read falls
    /// back to a full scan — slower than the full index the partial one replaced.
    fn assert_no_block_scan(steps: &[String]) {
        assert!(
            !steps
                .iter()
                .any(|step| step.contains("SCAN blocks") || step.contains("SCAN pages")),
            "a leaf must not fall back to scanning a table: {steps:?}"
        );
    }

    /// The index a leaf seeks must also still be the partial one. Without this half, dropping
    /// `WHERE … IS NOT NULL` would leave the plan test passing on the old full index — the seek
    /// still happens, only the savings are gone — so the lock would not catch the regression it
    /// exists for.
    fn assert_partial(conn: &Connection, index: &str) {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("{index} must exist: {error}"));
        assert!(
            sql.contains("WHERE") && sql.contains("IS NOT NULL"),
            "{index} must be partial on its sparse column, got: {sql}"
        );
    }

    #[test]
    fn a_task_leaf_seeks_the_marker_index() {
        let conn = Connection::open_in_memory().expect("open");
        crate::index::schema::init(&conn).expect("schema");
        assert_partial(&conn, "blocks_by_task_marker");
        for (sql, params) in [
            (
                "SELECT id FROM blocks WHERE task_marker IN (?)",
                vec![Value::Text("TODO".into())],
            ),
            (
                "SELECT id FROM blocks WHERE task_marker IN (?, ?)",
                vec![Value::Text("TODO".into()), Value::Text("DOING".into())],
            ),
        ] {
            let steps = plan(&conn, sql, &params);
            assert!(
                steps
                    .iter()
                    .any(|step| step.contains("blocks_by_task_marker")),
                "a task lookup must seek its index: {steps:?}"
            );
            assert_no_block_scan(&steps);
        }
    }

    #[test]
    fn a_priority_leaf_seeks_the_priority_index() {
        let conn = Connection::open_in_memory().expect("open");
        crate::index::schema::init(&conn).expect("schema");
        assert_partial(&conn, "blocks_by_priority");
        let steps = plan(
            &conn,
            "SELECT id FROM blocks WHERE priority IN (?)",
            &[Value::Text("A".into())],
        );
        assert!(
            steps.iter().any(|step| step.contains("blocks_by_priority")),
            "a priority lookup must seek its index: {steps:?}"
        );
        assert_no_block_scan(&steps);
    }

    #[test]
    fn a_between_leaf_seeks_the_journal_date_index() {
        let conn = Connection::open_in_memory().expect("open");
        crate::index::schema::init(&conn).expect("schema");
        assert_partial(&conn, "pages_by_journal_date");
        let steps = plan(
            &conn,
            "SELECT id FROM pages WHERE kind = 'journal' AND journal_date BETWEEN ? AND ?",
            &[Value::Text("a".into()), Value::Text("b".into())],
        );
        assert!(
            steps
                .iter()
                .any(|step| step.contains("pages_by_journal_date")),
            "a date range must seek its index: {steps:?}"
        );
        assert_no_block_scan(&steps);
    }

    #[test]
    fn a_property_value_denotes_a_reference_a_list_element_or_a_scalar() {
        assert!(property_value_matches("[[book]]", "type", Some("book")));
        assert!(property_value_matches(
            "[[book|reading]]",
            "type",
            Some("book")
        ));
        assert!(property_value_matches("#book", "type", Some("book")));
        assert!(property_value_matches("[[Book]]", "type", Some("book")));
        assert!(property_value_matches("a, b", "tags", Some("b")));
        assert!(property_value_matches("\"quoted\"", "type", Some("quoted")));
        assert!(property_value_matches("plain", "type", Some("plain")));
        assert!(
            !property_value_matches("a, b", "type", Some("b")),
            "only the deliberately list-valued keys split on commas"
        );
        assert!(
            !property_value_matches("liked this book", "description", Some("liked")),
            "a word that names no page and no list element is not a match"
        );
    }

    #[test]
    fn an_offset_that_leaves_the_calendar_is_a_query_error() {
        let ctx = QueryContext::new(JournalDate::new(9999, 12, 31).expect("day"), 10);
        let operand = DateOperand::OffsetDays(1);
        let conn = Connection::open_in_memory().expect("open");
        let error = resolve_day(&conn, &operand, &ctx).expect_err("an impossible day");
        assert!(matches!(
            error,
            QueryExecError::Query(QueryError::UnresolvedDate { .. })
        ));
    }
}
