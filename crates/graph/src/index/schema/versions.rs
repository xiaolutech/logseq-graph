use super::test_support::*;
use super::*;
use crate::index::MAX_BLOCK_DEPTH;

#[test]
fn the_application_id_is_a_fixed_nonzero_marker() {
    // The value is a durable file identity, not a tunable: every index on disk carries it, and a
    // silent change would make each of them look like another application's database and be
    // refused instead of opened.
    assert_eq!(APPLICATION_ID, 0x4C53_4752);
    assert_ne!(APPLICATION_ID, 0, "zero means unmarked, not this project");
}

#[test]
fn fresh_schema_starts_at_revision_zero_with_an_instance_id() {
    let conn = open();
    let metadata = read_metadata(&conn).expect("read metadata");

    assert_eq!(metadata.revision(), 0);
    assert_eq!(metadata.instance_id().len(), 32);
    assert!(metadata
        .instance_id()
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(read_metadata(&conn).expect("read again"), metadata);
}

#[test]
fn a_version_17_database_is_refused_instead_of_searched_without_the_cjk_index() {
    let conn = open();
    conn.execute_batch(
        "DROP TRIGGER block_search_insert;
         DROP TRIGGER block_search_delete;
         DROP TRIGGER block_search_update;
         DROP TABLE block_search_cjk;
         UPDATE graph SET schema_version = 17 WHERE id = 1;",
    )
    .expect("simulate a database before the CJK index");
    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, 17);
            assert_eq!(stored_parser, PARSER_VERSION);
        }
        other => panic!("expected a version 17 schema to be incompatible, got {other:?}"),
    }
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'block_search_cjk'",
            [],
            |row| row.get(0),
        )
        .expect("inspect schema");
    assert_eq!(tables, 0, "refuse before creating an empty CJK index");
}

#[test]
fn a_version_16_database_is_refused_instead_of_searched_without_fts() {
    let conn = open();
    conn.execute_batch(
        "DROP TRIGGER block_search_insert;
         DROP TRIGGER block_search_delete;
         DROP TRIGGER block_search_update;
         DROP TABLE block_search;
         UPDATE graph SET schema_version = 16 WHERE id = 1;",
    )
    .expect("simulate a database before FTS");

    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, 16);
            assert_eq!(stored_parser, PARSER_VERSION);
        }
        other => panic!("expected a version 16 schema to be incompatible, got {other:?}"),
    }
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'block_search'",
            [],
            |row| row.get(0),
        )
        .expect("inspect schema");
    assert_eq!(tables, 0, "refuse before creating an empty search index");
}

#[test]
fn a_version_11_database_is_refused_instead_of_read_without_task_facts() {
    // Task markers and priorities existed in the files all along, but a version 11 database
    // stores no column for them: every block would read as not a task. Nothing in those rows
    // distinguishes a plain block from a `TODO` one, so the stored version is the only signal,
    // and a rebuild from the files is the only way to fill the two columns (ADR-0002).
    let conn = open();
    conn.execute("UPDATE graph SET schema_version = 11 WHERE id = 1", [])
        .expect("write a version 11 header");

    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, 11);
            assert_eq!(stored_parser, PARSER_VERSION);
        }
        other => panic!("expected a version 11 schema to be incompatible, got {other:?}"),
    }
}

#[test]
fn a_version_12_database_is_refused_and_the_rebuild_makes_the_indexes_partial() {
    // Version 13 only changed the shape of four index trees, not the rows. `CREATE INDEX IF NOT
    // EXISTS` is why the version has to move: against a version 12 file the same four
    // statements find their own indexes already there and do nothing, so the stored database
    // would keep the full tree forever. Refusing it is what actually delivers the change, and
    // this test is the proof that the rebuilt indexes are the partial ones.
    let conn = open();
    conn.execute("UPDATE graph SET schema_version = 12 WHERE id = 1", [])
        .expect("write a version 12 header");

    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, 12);
            assert_eq!(stored_parser, PARSER_VERSION);
        }
        other => panic!("expected a version 12 schema to be incompatible, got {other:?}"),
    }

    // The refusal above happened before any DDL, so the file still holds the old header and no
    // new index; a fresh database is what a rebuild produces.
    let rebuilt = open();
    for index in [
        "blocks_by_task_marker",
        "blocks_by_priority",
        "blocks_by_durable_id",
        "pages_by_journal_date",
    ] {
        let sql: String = rebuilt
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("{index} must exist after the rebuild: {error}"));
        assert!(
            sql.contains("WHERE"),
            "{index} must be a partial index after the rebuild, got: {sql}"
        );
        assert!(
            sql.contains("IS NOT NULL"),
            "{index} must be partial on the sparse column, got: {sql}"
        );
    }
}

#[test]
fn previous_schema_version_is_rejected_instead_of_accepted() {
    let conn = Connection::open_in_memory().expect("open in-memory database");
    conn.execute_batch(V1_SCHEMA)
        .expect("create version 1 schema");
    conn.execute(
        "INSERT INTO graph (id, instance_id, schema_version, parser_version, revision)
         VALUES (1, 'version-one', 1, ?1, 0)",
        params![PARSER_VERSION],
    )
    .expect("record version 1 metadata");
    conn.execute(
        "INSERT INTO pages (name, block_count, revision) VALUES ('p', 1, 0)",
        [],
    )
    .expect("insert page");
    // Version 1 had no depth CHECK, so it stored a row this build cannot read back. Without
    // the version bump `init` would accept the database and leave that row unreachable.
    conn.execute(
        "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                             marker_kind, heading_level)
         VALUES (1, NULL, 0, ?1, 0, 'bullet', NULL)",
        params![MAX_BLOCK_DEPTH + 1],
    )
    .expect("version 1 accepted a block above the current depth limit");

    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, 1);
            assert_eq!(stored_parser, PARSER_VERSION);
        }
        other => panic!("expected the version 1 schema to be incompatible, got {other:?}"),
    }
}

#[test]
fn a_database_from_before_facts_existed_is_rebuilt_instead_of_read_as_fact_free() {
    // Every table this build creates also exists in a version 3 database, so the DDL below
    // succeeds on one and the fact tables are simply empty afterwards. Every page would then
    // read as having no properties, refs, or tags, and nothing in the rows would say so. The
    // stored version is the only signal, so it has to be the reason the file is refused.
    let conn = Connection::open_in_memory().expect("open in-memory database");
    conn.execute_batch(V1_SCHEMA)
        .expect("create an older schema");
    conn.execute(
        "INSERT INTO graph (id, instance_id, schema_version, parser_version, revision)
         VALUES (1, 'before-facts', 3, ?1, 7)",
        params![PARSER_VERSION],
    )
    .expect("record version 3 metadata");

    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, 3);
            assert_eq!(stored_parser, PARSER_VERSION);
        }
        other => panic!("expected a version 3 schema to be incompatible, got {other:?}"),
    }

    let facts: i64 = conn
        .query_row("SELECT COUNT(*) FROM page_refs", [], |row| row.get(0))
        .expect("the fact tables exist and are empty");
    assert_eq!(facts, 0, "creating the tables is not what refuses the file");
}

#[test]
fn version_mismatch_requires_a_rebuild() {
    let conn = open();
    conn.execute("UPDATE graph SET parser_version = 0 WHERE id = 1", [])
        .expect("downgrade stored parser version");

    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, SCHEMA_VERSION);
            assert_eq!(stored_parser, 0);
        }
        other => panic!("expected an incompatible schema error, got {other:?}"),
    }
}

#[test]
fn a_version_10_database_is_refused_instead_of_read_without_ephemeral_ids() {
    // Version 11 added a column no file can reconstruct: an ephemeral id exists only in the
    // index, so a version 10 database holds every page and block of this build's rows except the
    // identity of 98.8% of them. Migrating would mean inventing those values while the rows they
    // describe are already there; a rebuild mints them from the files, which is the same answer
    // every other version bump gets (ADR-0002, ADR-0012).
    let conn = open();
    conn.execute("UPDATE graph SET schema_version = 10 WHERE id = 1", [])
        .expect("write a version 10 header");

    match init(&conn) {
        Err(IndexError::Incompatible {
            stored_schema,
            stored_parser,
        }) => {
            assert_eq!(stored_schema, 10);
            assert_eq!(stored_parser, PARSER_VERSION);
        }
        other => panic!("expected a version 10 schema to be incompatible, got {other:?}"),
    }
}

/// The `blocks` DDL shipped as schema version 1, kept verbatim so the compatibility test
/// covers what version 1 actually accepted instead of a paraphrase of it.
const V1_SCHEMA: &str = "\
CREATE TABLE graph (
id             INTEGER PRIMARY KEY CHECK (id = 1),
instance_id    TEXT    NOT NULL,
schema_version INTEGER NOT NULL,
parser_version INTEGER NOT NULL,
revision       INTEGER NOT NULL
) STRICT;

CREATE TABLE pages (
id          INTEGER PRIMARY KEY,
name        TEXT    NOT NULL UNIQUE,
block_count INTEGER NOT NULL,
revision    INTEGER NOT NULL
) STRICT;

CREATE TABLE blocks (
id            INTEGER PRIMARY KEY,
page_id       INTEGER NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
parent_id     INTEGER          REFERENCES blocks (id) ON DELETE CASCADE,
preorder      INTEGER NOT NULL,
depth         INTEGER NOT NULL,
sibling_index INTEGER NOT NULL,
marker_kind   TEXT    NOT NULL CHECK (marker_kind IN ('bullet', 'heading')),
heading_level INTEGER          CHECK (heading_level BETWEEN 1 AND 6),
UNIQUE (page_id, preorder)
) STRICT;
";
