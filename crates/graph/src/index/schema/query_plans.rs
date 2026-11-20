use super::test_support::*;

#[test]
fn reverse_lookups_search_an_index_instead_of_scanning_the_table() {
    // A backlinks read asks which rows point at one target, on a table that grows with the whole
    // graph. Without these indexes every such read is a full scan, which the plan string makes
    // visible: a regression here is a missing `CREATE INDEX`, not a slow machine.
    //
    // The queries are written the way a reader would write them, with a plain `=`, so this also
    // pins the case-insensitive collation to the column: a query that needs to repeat
    // `COLLATE NOCASE` to match would mean the next reader can silently lose the index.
    let conn = open();
    let cases = [
        (
            "page_refs",
            "page_refs_by_target",
            "SELECT block_id FROM page_refs WHERE target_key = 'x'",
        ),
        (
            "block_refs",
            "block_refs_by_target",
            "SELECT block_id FROM block_refs WHERE target_uuid = 'x'",
        ),
        (
            "tags",
            "tags_by_name",
            "SELECT block_id FROM tags WHERE name_key = 'x'",
        ),
    ];
    for (table, index, sql) in cases {
        let plan = query_plan(&conn, sql);
        assert!(
            plan.iter().any(|step| step.contains(index)),
            "{table} must be read through {index}: {plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|step| step.contains(&format!("SCAN {table}"))),
            "{table} must not be scanned in full: {plan:?}"
        );
    }
}

#[test]
fn a_block_delete_finds_children_through_an_index_instead_of_scanning_the_table() {
    // `blocks.parent_id` is a self-reference with `ON DELETE CASCADE`, so deleting one block
    // makes SQLite look for its children. This protects the cost of that foreign key action,
    // not a read: without the index every deleted block scans the largest table in the index,
    // and one page replacement costs `blocks on the page x blocks in the graph`.
    let conn = open();
    // The cascade is only compiled into the statement when foreign key enforcement is on, which
    // is the state `Index::open_at` puts every connection in.
    conn.pragma_update(None, "foreign_keys", true)
        .expect("enable foreign keys");

    let delete = query_plan(&conn, "DELETE FROM blocks WHERE page_id = 1");
    assert!(
        delete.iter().any(|step| step.contains("blocks_by_parent")),
        "the cascade must search blocks_by_parent: {delete:?}"
    );
    assert!(
        !delete.iter().any(|step| step.contains("SCAN blocks")),
        "no step of the cascade may scan blocks in full: {delete:?}"
    );

    let children = query_plan(&conn, "SELECT id FROM blocks WHERE parent_id = 1");
    assert!(
        children
            .iter()
            .any(|step| step.contains("blocks_by_parent")),
        "a child lookup must search blocks_by_parent: {children:?}"
    );
}

#[test]
fn a_page_lookup_by_path_searches_an_index_instead_of_scanning_pages() {
    // The per-path refresh asks which page one file backs. A full scan of `pages` per path
    // would make a one-file refresh cost the whole page list. `pages.relative_path` is UNIQUE,
    // so SQLite already carries the automatic index this needs; the plan pins that a reader
    // who removes the constraint (or replaces the query with one the index cannot serve) sees
    // a failing test instead of a slow refresh.
    let conn = open();
    let plan = query_plan(
        &conn,
        "SELECT name FROM pages WHERE relative_path = 'pages/p.md'",
    );
    assert!(
        plan.iter().any(|step| {
            step.contains("SEARCH pages USING") && step.contains("relative_path=?")
        }),
        "the path lookup must seek on pages.relative_path: {plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.contains("SCAN pages")),
        "pages must not be scanned in full: {plan:?}"
    );
}

#[test]
fn the_task_and_property_lookups_search_an_index_instead_of_scanning() {
    // These are the reads the query slice will run: one task state or priority over the whole
    // block table, one property by name across owners, one day among the journals. Each is one
    // equality, so each has to be one seek; a missing `CREATE INDEX` would make it a full scan
    // of the largest table in the index and no other test would notice.
    let conn = open();
    let cases = [
        (
            "blocks",
            "blocks_by_task_marker",
            "SELECT id FROM blocks WHERE task_marker = 'TODO'",
        ),
        (
            "blocks",
            "blocks_by_priority",
            "SELECT id FROM blocks WHERE priority = 'A'",
        ),
        (
            "page_properties",
            "page_properties_by_name",
            "SELECT page_id FROM page_properties WHERE normalized_key = 'x'",
        ),
        (
            "block_properties",
            "block_properties_by_name",
            "SELECT block_id FROM block_properties WHERE normalized_key = 'x'",
        ),
        (
            "pages",
            "pages_by_journal_date",
            "SELECT name FROM pages WHERE journal_date = '2026-08-13'",
        ),
    ];
    for (table, index, sql) in cases {
        let plan = query_plan(&conn, sql);
        assert!(
            plan.iter().any(|step| step.contains(index)),
            "{table} must be read through {index}: {plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|step| step.contains(&format!("SCAN {table}"))),
            "{table} must not be scanned in full: {plan:?}"
        );
    }
}

#[test]
fn a_backlink_join_searches_on_both_sides_instead_of_scanning() {
    // The query a backlinks read runs: find the target page by key, then the rows that point at
    // it, then the block and page they came from. Every step has to be a seek. This is the shape
    // that was broken while `pages.name` was compared with an ASCII-folding ref column: the
    // join matched nothing and scanned, which no plan-free test would have noticed.
    let conn = open();
    let plan = query_plan(
        &conn,
        "SELECT source.name, blocks.preorder
           FROM pages AS target
           JOIN page_refs ON page_refs.target_key = target.name_key
           JOIN blocks ON blocks.id = page_refs.block_id
           JOIN pages AS source ON source.id = blocks.page_id
          WHERE target.name_key = 'x'",
    );

    assert!(
        plan.iter().any(|step| step.contains("page_refs_by_target")),
        "the ref side must be read through its target index: {plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.contains("SCAN")),
        "no step of a backlink read may scan: {plan:?}"
    );
}
