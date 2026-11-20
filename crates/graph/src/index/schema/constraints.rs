use super::test_support::*;
use super::*;
use crate::index::MAX_BLOCK_DEPTH;
use rusqlite::ErrorCode;

#[test]
fn durable_id_is_optional_but_fixed_length_when_present() {
    let conn = open();
    insert_page(&conn, "p").expect("insert page");
    conn.execute(
        "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                             marker_kind, heading_level, text, ephemeral_id)
         VALUES (1, NULL, 0, 0, 0, 'bullet', NULL, '', '01234567-89ab-cdef-0123-456789abcdef')",
        [],
    )
    .expect("a block that states no id:: carries the id the projection minted");
    let stored: Option<String> = conn
        .query_row(
            "SELECT durable_id FROM blocks WHERE preorder = 0",
            [],
            |row| row.get(0),
        )
        .expect("read durable_id");
    assert_eq!(
        stored, None,
        "absent id:: must be NULL, not an empty string"
    );

    let uuid = "01234567-89ab-cdef-0123-456789abcdef";
    conn.execute(
        "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                             marker_kind, heading_level, text, durable_id)
         VALUES (1, NULL, 1, 0, 1, 'bullet', NULL, '', ?1)",
        [uuid],
    )
    .expect("a 36-character value is the shape the projection stores");

    let error = conn
        .execute(
            "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                 marker_kind, heading_level, text, durable_id)
             VALUES (1, NULL, 2, 0, 2, 'bullet', NULL, '', 'not-a-uuid')",
            [],
        )
        .expect_err("a non-NULL durable_id must be 36 characters");
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));
}

#[test]
fn blocks_state_exactly_one_identity() {
    // Neither address nor two: a block with both would have two identities that could disagree
    // about which block this is, and one with neither could not be named at all. Both states are
    // unreadable, so the database refuses them rather than letting a projection defect store
    // something no reader can interpret (ADR-0012).
    let conn = open();
    insert_page(&conn, "p").expect("insert page");
    let uuid = "01234567-89ab-cdef-0123-456789abcdef";

    let neither = conn
        .execute(
            "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                 marker_kind, heading_level, text)
             VALUES (1, NULL, 0, 0, 0, 'bullet', NULL, '')",
            [],
        )
        .expect_err("a block with no identity at all must be refused");
    assert!(matches!(
        neither,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));

    let both = conn
        .execute(
            "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                 marker_kind, heading_level, text, durable_id, ephemeral_id)
             VALUES (1, NULL, 1, 0, 1, 'bullet', NULL, '', ?1, ?1)",
            [uuid],
        )
        .expect_err("a block with two identities must be refused");
    assert!(matches!(
        both,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));
}

#[test]
fn one_ephemeral_id_names_at_most_one_block() {
    // Durable ids are not unique here, because the file is the authority on whether two blocks
    // state one. An ephemeral id is minted by this library, so a repeated one is a defect rather
    // than an authored fact, and the schema is what makes "one id names one block" true instead
    // of merely likely (ADR-0012).
    let conn = open();
    insert_page(&conn, "p").expect("insert page");
    let uuid = "01234567-89ab-cdef-0123-456789abcdef";
    conn.execute(
        "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                             marker_kind, heading_level, text, ephemeral_id)
         VALUES (1, NULL, 0, 0, 0, 'bullet', NULL, '', ?1)",
        [uuid],
    )
    .expect("first block takes the id");

    let error = conn
        .execute(
            "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                 marker_kind, heading_level, text, ephemeral_id)
             VALUES (1, NULL, 1, 0, 1, 'bullet', NULL, '', ?1)",
            [uuid],
        )
        .expect_err("a second block must not take the same ephemeral id");
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));
}

#[test]
fn schema_columns_hold_only_derived_state() {
    let conn = open();

    assert_eq!(
        columns(&conn, "graph"),
        [
            "id",
            "instance_id",
            "schema_version",
            "parser_version",
            "revision",
            "config_signature"
        ]
    );
    // A page keeps the name as written for display, its key for identity, which kind of page it
    // is, and the path of the file it was projected from, which is the only link between the two
    // tables below.
    assert_eq!(
        columns(&conn, "pages"),
        [
            "id",
            "name",
            "name_key",
            "kind",
            "journal_date",
            "relative_path",
            "block_count",
            "revision"
        ]
    );
    assert_eq!(
        columns(&conn, "blocks"),
        [
            "id",
            "page_id",
            "parent_id",
            "preorder",
            "depth",
            "sibling_index",
            "marker_kind",
            "heading_level",
            "text",
            "durable_id",
            "ephemeral_id",
            "task_marker",
            "priority"
        ]
    );
    // The baseline stores the observation of a file and a fingerprint of its text. It holds no
    // page name, no page kind, and no source text: the path and the files themselves answer
    // those.
    assert_eq!(
        columns(&conn, "file_baseline"),
        ["relative_path", "size", "modified_ns", "content_hash"]
    );
    // Facts hold the text a property, ref, or tag was written as, plus the normalized key of a
    // property. A block's own text is stored too, but no table holds a source line, a line
    // number, or a source map: those stay in the file, and every row here is reconstructible by
    // parsing it again.
    assert_eq!(
        columns(&conn, "page_properties"),
        ["page_id", "ordinal", "key", "normalized_key", "value"]
    );
    assert_eq!(
        columns(&conn, "block_properties"),
        ["block_id", "ordinal", "key", "normalized_key", "value"]
    );
    assert_eq!(
        columns(&conn, "page_refs"),
        ["block_id", "target_name", "target_key"]
    );
    // A block ref needs no key column: the target is an already-lowercased hexadecimal UUID.
    assert_eq!(columns(&conn, "block_refs"), ["block_id", "target_uuid"]);
    assert_eq!(columns(&conn, "tags"), ["block_id", "name", "name_key"]);
    // The invariant is about source representation, not about the word "text": block text is
    // derived content, while a raw line, a line number, or a source map would make the index a
    // second copy of the Markdown. Naming any of them would be the regression this catches.
    // `durable_id` is the UUID a block states in its own text, so it passes too: it is a fact
    // the parse extracted, not a pointer into the file. `ephemeral_id` is a value this build
    // invented and stored, which is also derived state: the file reconstructs nothing about it,
    // so a rebuild replaces it (ADR-0012).
    for table in [
        "graph",
        "pages",
        "blocks",
        "file_baseline",
        "page_properties",
        "block_properties",
        "page_refs",
        "block_refs",
        "tags",
    ] {
        for column in columns(&conn, table) {
            let column = column.to_ascii_lowercase();
            assert!(
                !column.contains("line")
                    && !column.contains("source")
                    && !column.contains("span")
                    && column != "offset",
                "{table}.{column} would store source representation, not derived state"
            );
        }
    }
}

#[test]
fn pages_reject_a_kind_this_build_could_not_read_back() {
    let conn = open();
    let store = |name: &str, kind: &str, journal_date: Option<&str>| {
        let relative_path = format!("pages/{name}.md");
        conn.execute(
            "INSERT INTO file_baseline (relative_path, size, modified_ns, content_hash)
             VALUES (?1, 1, NULL, 0)",
            [&relative_path],
        )?;
        conn.execute(
            "INSERT INTO pages (name, name_key, kind, journal_date, relative_path, block_count,
                                revision)
             VALUES (?1, ?1, ?2, ?3, ?4, 0, 0)",
            params![name, kind, journal_date, relative_path],
        )
    };

    for (kind, date) in [("page", None), ("journal", Some("2026-08-13"))] {
        store(kind, kind, date).expect("known kind");
    }
    for kind in ["Page", "block", ""] {
        let error = store(&format!("unknown-{kind}"), kind, None)
            .expect_err("unknown kind must be rejected");
        assert!(
            matches!(
                error,
                rusqlite::Error::SqliteFailure(error, _)
                    if error.code == ErrorCode::ConstraintViolation
            ),
            "a kind this build cannot read back must be a constraint failure, got {error:?}"
        );
    }
}

#[test]
fn a_page_row_states_its_day_only_when_it_is_a_journal() {
    // The two cells are one fact, so the database refuses either half alone: a journal without a
    // day is a page whose identity nothing can state, and a regular page with a day is a day no
    // name describes. Both would read back as a value this build cannot answer for.
    let conn = open();
    let store = |name: &str, kind: &str, journal_date: Option<&str>| {
        let relative_path = format!("pages/{name}.md");
        conn.execute(
            "INSERT INTO file_baseline (relative_path, size, modified_ns, content_hash)
             VALUES (?1, 1, NULL, 0)",
            [&relative_path],
        )?;
        conn.execute(
            "INSERT INTO pages (name, name_key, kind, journal_date, relative_path, block_count,
                                revision)
             VALUES (?1, ?1, ?2, ?3, ?4, 0, 0)",
            params![name, kind, journal_date, relative_path],
        )
    };

    for (kind, date) in [("journal", None), ("page", Some("2026-08-13"))] {
        let error = store(&format!("mismatched-{kind}-{}", date.is_some()), kind, date)
            .expect_err("a kind and a day that disagree must be rejected");
        assert!(
            matches!(
                error,
                rusqlite::Error::SqliteFailure(error, _)
                    if error.code == ErrorCode::ConstraintViolation
            ),
            "{kind} with date {date:?} must be a constraint failure, got {error:?}"
        );
    }

    store("both-journal", "journal", Some("2026-08-13")).expect("a journal and its day");
    store("no-day-page", "page", None).expect("a page without a day");
}

#[test]
fn the_stored_day_sorts_in_time_where_the_journal_name_sorts_as_text() {
    // This is the reason the column exists. A journal's `name` is `:journal/page-title-format`'s
    // rendering, so it is ordered as text: `Apr 19th, 2021` sorts before `Apr 1st, 2021`, and
    // two years interleave whenever their month names do. The stored `YYYY-MM-DD` spelling
    // sorts in time by itself, which is what a range or a chronological listing needs.
    let conn = open();
    let store = |name: &str, date: &str| {
        let relative_path = format!("journals/{date}.md");
        conn.execute(
            "INSERT INTO file_baseline (relative_path, size, modified_ns, content_hash)
             VALUES (?1, 1, NULL, 0)",
            [&relative_path],
        )?;
        conn.execute(
            "INSERT INTO pages (name, name_key, kind, journal_date, relative_path, block_count,
                                revision)
             VALUES (?1, ?2, 'journal', ?3, ?4, 0, 0)",
            params![
                name,
                crate::domain::page_name_key(name),
                date,
                relative_path
            ],
        )
    };
    for (name, date) in [
        ("Jan 5th, 2026", "2026-01-05"),
        ("Dec 31st, 2025", "2025-12-31"),
        ("Aug 13th, 2026", "2026-08-13"),
    ] {
        store(name, date).expect("insert a journal");
    }

    let ordered = |column: &str| -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT journal_date FROM pages WHERE kind = 'journal' ORDER BY {column}"
            ))
            .expect("prepare ordered read");
        stmt.query_map([], |row| row.get::<_, String>(0))
            .expect("query ordered days")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read ordered days")
    };

    assert_eq!(
        ordered("journal_date"),
        ["2025-12-31", "2026-01-05", "2026-08-13"],
        "the stored day orders journals by time"
    );
    assert_eq!(
        ordered("name_key"),
        ["2026-08-13", "2025-12-31", "2026-01-05"],
        "the name orders the same three by its text, which is not time"
    );
}

#[test]
fn constraints_reject_rows_this_build_could_not_read_back() {
    let conn = open();
    insert_page(&conn, "p").expect("insert page");

    // The deepest supported block is accepted, one level deeper is not.
    assert_eq!(MAX_BLOCK_DEPTH, 255);
    insert_block(&conn, MAX_BLOCK_DEPTH, "bullet", None).expect("deepest legal block");
    let too_deep = insert_block(&conn, MAX_BLOCK_DEPTH + 1, "bullet", None)
        .expect_err("depth above the limit must be rejected");
    assert!(matches!(
        too_deep,
        rusqlite::Error::SqliteFailure(error, _) if error.code == ErrorCode::ConstraintViolation
    ));

    // A marker and a heading level must agree, so no row can decode into an unknown marker.
    for (kind, level) in [
        ("bullet", Some(2)),
        ("heading", None),
        ("heading", Some(0)),
        ("heading", Some(7)),
        ("paragraph", None),
    ] {
        let error = insert_block(&conn, 0, kind, level)
            .expect_err("impossible marker combination must be rejected");
        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(error, _)
                if error.code == ErrorCode::ConstraintViolation
        ));
    }
    insert_block(&conn, 0, "heading", Some(6)).expect("legal heading");
}

#[test]
fn a_task_marker_or_priority_this_build_could_not_decode_is_refused() {
    // The two columns store `domain::TaskMarker` and `domain::Priority` spellings and a read
    // decodes them into those enums, so a value outside the set is a row nothing can answer for.
    // NULL stays allowed: most blocks are not tasks.
    let conn = open();
    insert_page(&conn, "p").expect("insert page");
    let store = |preorder: u32, task: Option<&str>, priority: Option<&str>| {
        let ephemeral_id = format!("{preorder:08x}-0000-0000-0000-000000000000");
        conn.execute(
            "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                 marker_kind, heading_level, text, ephemeral_id, task_marker,
                                 priority)
             VALUES (1, NULL, ?1, 0, ?1, 'bullet', NULL, '', ?2, ?3, ?4)",
            params![preorder, ephemeral_id, task, priority],
        )
    };

    for (task, priority) in [
        (None, None),
        (Some("TODO"), None),
        (Some("CANCELED"), Some("C")),
        (None, Some("B")),
    ] {
        store(0, task, priority).expect("a value this build decodes is stored");
        conn.execute("DELETE FROM blocks", []).expect("clear");
    }

    for (task, priority) in [
        (Some("todo"), None),
        (Some("CANCELLED"), None),
        (Some(""), None),
        (Some("TODO"), Some("D")),
        (Some("TODO"), Some("a")),
        (None, Some("A ")),
    ] {
        let error = store(0, task, priority)
            .expect_err("a value outside the two enumerations must be refused");
        assert!(
            matches!(
                error,
                rusqlite::Error::SqliteFailure(error, _)
                    if error.code == ErrorCode::ConstraintViolation
            ),
            "{task:?}/{priority:?} must be a constraint failure, got {error:?}"
        );
        conn.execute("DELETE FROM blocks", []).expect("clear");
    }
}

#[test]
fn two_pages_that_differ_only_in_case_cannot_both_be_stored() {
    // `page_name_key` maps both spellings to one key, and the unique index on that key is what
    // keeps a read model from holding two answers to one question. `replace` deletes by key
    // before inserting, so this only fires on a caller that writes rows itself, which is exactly
    // the bug the constraint has to catch.
    let conn = open();
    insert_page(&conn, "Ä").expect("first spelling of the page");

    let error = insert_page(&conn, "ä")
        .expect_err("the second spelling is the same page and must be rejected");
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));
}

#[test]
fn a_page_row_needs_the_baseline_of_the_file_it_came_from() {
    // The pairing of a page with its file is one to one and is enforced by the database, not by
    // the order a caller happens to write in.
    let conn = open();

    let missing = conn
        .execute(
            "INSERT INTO pages (name, name_key, kind, relative_path, block_count, revision)
             VALUES ('p', 'p', 'page', 'pages/p.md', 0, 0)",
            [],
        )
        .expect_err("a page without the baseline of its file must be rejected");
    assert!(matches!(
        missing,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));

    insert_page(&conn, "p").expect("the pair is accepted");
    let stored: String = conn
        .query_row(
            "SELECT relative_path FROM pages WHERE name = 'p'",
            [],
            |row| row.get(0),
        )
        .expect("read the stored path");
    assert_eq!(stored, "pages/p.md");

    // A second page cannot claim a file that already holds one.
    let duplicate = conn
        .execute(
            "INSERT INTO pages (name, name_key, kind, relative_path, block_count, revision)
             VALUES ('other', 'other', 'page', 'pages/p.md', 0, 0)",
            [],
        )
        .expect_err("one file holds one page");
    assert!(matches!(
        duplicate,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));

    // Removing the observation while its page remains is refused: the scan removes the page
    // first, and NO ACTION is what makes that order mandatory rather than customary.
    let kept_baseline = conn
        .execute(
            "DELETE FROM file_baseline WHERE relative_path = 'pages/p.md'",
            [],
        )
        .expect_err("a baseline cannot vanish from under its page");
    assert!(matches!(
        kept_baseline,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation
    ));

    conn.execute("DELETE FROM pages WHERE name = 'p'", [])
        .expect("remove the page first");
    conn.execute(
        "DELETE FROM file_baseline WHERE relative_path = 'pages/p.md'",
        [],
    )
    .expect("the baseline can go once its page is gone");
}
