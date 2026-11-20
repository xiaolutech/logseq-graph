use super::*;

pub(super) fn open() -> Connection {
    crate::index::Index::open_in_memory()
        .expect("open in-memory index")
        .conn
}

pub(super) fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .expect("prepare column query");
    let names = stmt
        .query_map([table], |row| row.get::<_, String>(0))
        .expect("query columns")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read columns");
    names
}

/// Insert one block, returning the raw result so constraint violations stay visible.
pub(super) fn insert_block(
    conn: &Connection,
    depth: u32,
    marker_kind: &str,
    heading_level: Option<u32>,
) -> rusqlite::Result<usize> {
    // Every block row states exactly one identity, so the helper mints a shape-valid ephemeral
    // id per insertion from the preorder: these tests are about the other columns, and the
    // identity rule has its own test below.
    let ephemeral_id = format!("{depth:08x}-0000-0000-0000-000000000000");
    conn.execute(
        "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                             marker_kind, heading_level, text, ephemeral_id)
         VALUES (1, NULL, ?1, ?2, 0, ?3, ?4, '', ?5)",
        params![depth, depth, marker_kind, heading_level, ephemeral_id],
    )
}

/// Insert one page row together with the baseline its foreign key requires.
pub(super) fn insert_page(conn: &Connection, name: &str) -> rusqlite::Result<usize> {
    let relative_path = format!("pages/{name}.md");
    conn.execute(
        "INSERT INTO file_baseline (relative_path, size, modified_ns, content_hash)
         VALUES (?1, 0, NULL, 0)",
        [&relative_path],
    )?;
    conn.execute(
        "INSERT INTO pages (name, name_key, kind, relative_path, block_count, revision)
         VALUES (?1, ?2, 'page', ?3, 0, 0)",
        params![name, crate::domain::page_name_key(name), relative_path],
    )
}

/// The steps SQLite reports for one query, as `detail` strings.
pub(super) fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare plan query");
    let steps = stmt
        .query_map([], |row| row.get::<_, String>(3))
        .expect("query plan")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read plan");
    steps
}
