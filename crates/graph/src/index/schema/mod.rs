//! Schema definition, versioning, and graph instance metadata.
//!
//! The schema stores derived state only: page and block structure, the content facts
//! `document::facts` reads out of a block's own text, and each block's text as that same layer
//! reconstructs it. Source lines, line numbers, and source maps have no column here, because files
//! remain authoritative and every row must be reconstructible by parsing the files again (ADR-0001,
//! ADR-0002, ADR-0007).

use rusqlite::{params, Connection, OptionalExtension};

use crate::domain::GraphMetadata;

use super::IndexError;

/// Version of the SQL schema in this build.
///
/// A stored database with a different version is not migrated; it is reported as incompatible so
/// the caller rebuilds the derived state from the files.
///
/// Version history:
/// - 1: first shape of `graph`, `pages`, and `blocks`.
/// - 2: `blocks` gained `CHECK (depth BETWEEN 0 AND 255)` and the combined marker/heading-level
///   CHECK, so rows version 1 accepted can be unreadable by this build.
/// - 3: added `file_baseline`, so a startup scan can skip a file whose observed stat and content
///   fingerprint still match the last projection instead of re-parsing the whole graph. Version 2
///   databases have no baseline at all, so they would be projected in full anyway; a rebuild is
///   cheaper than a migration (ADR-0002).
/// - 4: added `page_properties`, `block_properties`, `page_refs`, `block_refs`, and `tags`, so one
///   page replacement stores the content facts `document::facts` extracts from the same parse. It
///   also gave a name an identity column: `pages.name_key`, `page_refs.target_key`, and
///   `tags.name_key` hold `domain::page_name_key` of the name beside the name as written, so SQL
///   compares two names the way every other layer does. A version 3 database holds none of those
///   rows, and nothing in it distinguishes a page that has no properties from a page projected
///   before this build existed, so reading it would silently report every page as fact-free. A
///   rebuild from the files is the only answer that is not a guess (ADR-0002).
/// - 5: `blocks` gained `text`, the block's own content as `document::facts` reconstructs it from
///   the same parse: marker and alignment indentation removed, continuation lines joined. It is
///   derived state, not a raw source line, and it exists so one bounded query can search the graph
///   without reading files (ADR-0007). A version 4 database has no such column, and a rebuild from
///   the files is the only way to fill it, so it is refused rather than migrated.
/// - 6: `blocks` gained `durable_id`, the `id::` UUID a block states, so a durable block target
///   resolves without scanning text (ADR-0005). The value is validated by `domain::BlockId` at
///   projection time and NULL when the block states no `id::`; the SQL CHECK only rejects a
///   non-NULL value that is not 36 characters. A version 5 database has no such column and a
///   rebuild from the files is the only way to fill it, so it is refused rather than migrated.
/// - 7: `pages` gained `relative_path`, the file the page row was projected from, as a foreign key
///   into `file_baseline`. Page rows and baselines were already written in one transaction and
///   paired one to one, but nothing in the database said so, so every "which file holds this page"
///   question re-derived the whole name-to-path map from the filesystem. A version 6 database has
///   no such column and no way to fill it without rediscovering the graph, so it is refused rather
///   than migrated.
/// - 8: added `write_intent`, the single-row record of the first multi-file operation, a
///   cross-page block move. File state alone cannot say what a two-file write was trying to reach,
///   so the intent pins both files and their content fingerprints before the first file is
///   replaced, and the multi-page commit clears it in the same transaction that replaces the pages
///   (ADR-0004). A version 7 database has no such table; a rebuild from the files projects them as
///   they are, which is exactly what the intent's absence means, so it is refused rather than
///   migrated.
/// - 9: `pages` gained `kind`, whether the page is a regular page or a journal, and
///   `file_baseline` lost `page_kind`, which held the same fact about the observation instead of
///   about the page. The baseline column was never a rule: its value is a function of
///   `relative_path`, and the scan's skip test already compares the stored fingerprint with the
///   text it just read, so the only reader of `page_kind` compared it with a kind derived from the
///   same path and could never disagree. A read that answers page metadata or a page listing now
///   states the kind without a join. A version 8 database has neither column shape and a rebuild
///   from the files is the only way to fill or drop them, so it is refused rather than migrated.
/// - 10: `pages` gained `journal_date`, the calendar day a journal is, stored as `YYYY-MM-DD`.
///   A journal's identity is that day, but its name is a rendering of it — `:journal/page-title-format`
///   can turn one day into any label — and nothing can read a day back out of a label. The day is
///   known once, from the file name parsing at discovery, so it is stored then; a read that needs
///   to order or filter journals by date has no other source. Version 9 has no such column and a
///   rebuild from the files is the only way to fill it, so it is refused rather than migrated.
/// - 11: `blocks` gained `ephemeral_id`, the random UUID the projection mints for a block that
///   states no `id::`, with a `UNIQUE (ephemeral_id)` index (ADR-0012). The column is the only
///   place an ephemeral block identity exists: no file holds one, so a version 10 database has no
///   value to carry over and every ephemeral id in it would have to be invented. A rebuild mints
///   them from the files, which is also what clears every id the old rows held, so version 10 is
///   refused rather than migrated.
/// - 12: `blocks` gained `task_marker` and `priority`, the two facts a block's text can lead with
///   (`domain::TaskMarker` and `domain::Priority`), so a task query filters two short columns
///   instead of scanning the text of every block. Both are derived from the text the row already
///   stores and both are NULL on a block that states neither, but a version 11 database holds no
///   row that says which is which, and the files are the only place to read it from. A rebuild is
///   the answer, so it is refused rather than migrated.
/// - 13: four indexes on columns that are NULL on most rows — `blocks.task_marker`,
///   `blocks.priority`, `blocks.durable_id`, `pages.journal_date` — became partial on
///   `<column> IS NOT NULL`, so a block or page that states nothing keeps no index entry. The rows
///   are the same rows and mean the same thing; only the shape of the index trees changed. The
///   version still had to move, because these indexes are created with `CREATE INDEX IF NOT
///   EXISTS`: against a version 12 file each statement finds an index of the same name and does
///   nothing, so the old full-tree index would survive silently and the change would apply to new
///   databases only. The version bump is what makes it land on someone else's file — `init`
///   refuses the stored version, the caller deletes this derived state, and the rebuild creates
///   the partial indexes (ADR-0002).
///
/// Version 14 records the configuration signature of the completed graph projection.
/// Version 15 moves recovery state into the durable graph journal and removes `write_intent`.
/// Version 16 adds derived page entities and page-name claims.
/// Version 17 adds the folded trigram FTS5 index for ranked content search.
/// Version 18 adds a CJK token index for short literal content searches.
pub(super) const SCHEMA_VERSION: i64 = 18;

/// Version of the page projection in this build.
///
/// Bump this when the meaning of a projected row changes, including any change to the block
/// structure that `document` parses.
///
/// Schema version 5 added one column whose value is produced by the same parse version 4 used:
/// `document::facts` always extracted a block's text, version 5 only started storing it. Schema
/// version 6 is the same shape of change: `document::facts` always extracted a block's `id::`
/// property, version 6 only started storing it as `blocks.durable_id`. Neither change altered what
/// a parse means, so this stays at 1. A rebuild of an older database is still required, but because
/// its rows are incomplete, not because they mean something else.
///
/// Schema version 11 is the third change of that shape: the projection always knew which blocks
/// state an `id::` and which do not, and version 11 only started storing a minted id for the ones
/// that do not (ADR-0012). Nothing a file says means anything different, so this stayed at 1.
///
/// It is 2 from schema version 12 on, for a different kind of change: the projection now reports
/// two facts about a block that it did not report before — the task marker its text leads with and
/// the priority that follows it (`domain::TaskMarker`, `domain::Priority`). The bytes did not move;
/// what they mean to this build did, so the same block at the same preorder is a value with more in
/// it than a version 11 row said. That is precisely what this number records, and the difference
/// from the bumps above is deliberate: storing an already-extracted fact leaves this at 1, a new
/// fact takes it to 2.
///
/// Schema version 13 does not move it. That version only changed which rows an index holds — four
/// sparse columns became partial indexes — and a block at a given preorder still projects to the
/// same facts. A file re-parsed by this build produces what version 12 produced.
///
/// It is 3 because the parse itself moved: a fenced code block whose info string holds a backtick no
/// longer opens a fence (CommonMark 4.5, measured against the official graph parser), so a page that
/// held one block there now holds a block per line and every one of those texts is a different
/// value. That is the case this number exists for: the bytes did not move, and what they project to
/// did.
///
/// It is 4 for the same kind of change, one step further: only `-` opens a block now. `*` and `+`
/// are valid CommonMark bullets and are text to Logseq, so a logbook line, a `curl -v` line, or a
/// code fragment stops being an outliner entry and becomes continuation text of the block above it
/// (or of the page's preamble). Block counts, block texts, and every preorder below such a line
/// change, and so does what a write is allowed to put in a continuation line.
/// It is 5 for the same reason the two parse fixes were 3 and 4, one layer out: a `#` inside the
/// label of a Markdown inline link is no longer a tag (measured against the pinned official graph
/// parser; see `docs/corpus-parity.md`). The bytes did not move and no block text changed, but the
/// tag rows a page projects to did — an index built by version 4 still holds the tag `2` that
/// `[Logseq Office Hours #2: ...](url)` used to mint — so a rebuild is what makes the derived state
/// agree with this build again.
/// Version 6 aligns page identity with the official parser, including titles and derived entities.
/// Version 7 matches the official bare-marker continuations, boundary blank lines, and unclosed
/// code fences. Both block texts and block structure can change without any source byte changing.
pub(super) const PARSER_VERSION: i64 = 7;

/// Marker written to the database header so a file can be recognized as this project's index.
///
/// The value spells `LSGR` (LogSeq GRaph) in ASCII, which makes it readable in a hex dump and keeps
/// it clear of the small integers applications use for toy files. It is deliberately unrelated to
/// [`SCHEMA_VERSION`]: the marker identifies the file, while the schema version identifies the shape
/// of the rows inside it, and an incompatible schema still triggers a rebuild of this same file.
/// Changing this constant would make every existing index look like another application's database.
pub(super) const APPLICATION_ID: i32 = 0x4C53_4752;

mod ddl;
use ddl::SCHEMA;
pub(super) use ddl::{DURABLE_ID_INDEX, EPHEMERAL_ID_INDEX, QUERY_INDEXES};

/// The raw metadata row, including the versions that decide compatibility.
struct StoredMetadata {
    instance_id: String,
    schema_version: i64,
    parser_version: i64,
    revision: u64,
}

/// Create the schema when it is missing, or verify an existing one.
///
/// An incompatible stored version is an error rather than a migration: the derived state is
/// disposable, so the caller deletes it and rebuilds from the files.
pub(super) fn init(conn: &Connection) -> Result<GraphMetadata, IndexError> {
    conn.execute_batch(SCHEMA)?;

    let stored = read_stored(conn)?;
    if let Some(stored) = &stored {
        if stored.schema_version != SCHEMA_VERSION || stored.parser_version != PARSER_VERSION {
            return Err(IndexError::Incompatible {
                stored_schema: stored.schema_version,
                stored_parser: stored.parser_version,
            });
        }
    }

    // See [`DURABLE_ID_INDEX`]: created only for a fresh or an already-compatible database, so an
    // incompatible one is refused by its version before any DDL names a column it does not have.
    conn.execute_batch(DURABLE_ID_INDEX)?;
    conn.execute_batch(EPHEMERAL_ID_INDEX)?;
    conn.execute_batch(QUERY_INDEXES)?;

    let Some(stored) = stored else {
        conn.execute(
            "INSERT INTO graph (id, instance_id, schema_version, parser_version, revision)
             VALUES (1, lower(hex(randomblob(16))), ?1, ?2, 0)",
            params![SCHEMA_VERSION, PARSER_VERSION],
        )?;
        return read_metadata(conn);
    };

    Ok(GraphMetadata::new(stored.instance_id, stored.revision))
}

/// Read the graph instance identity and the last committed revision.
pub(super) fn read_metadata(conn: &Connection) -> Result<GraphMetadata, IndexError> {
    let stored = read_stored(conn)?.ok_or(IndexError::CorruptRow {
        column: "graph",
        value: "missing singleton metadata row".to_owned(),
    })?;
    Ok(GraphMetadata::new(stored.instance_id, stored.revision))
}

fn read_stored(conn: &Connection) -> Result<Option<StoredMetadata>, IndexError> {
    let stored = conn
        .query_row(
            "SELECT instance_id, schema_version, parser_version, revision
               FROM graph WHERE id = 1",
            [],
            |row| {
                Ok(StoredMetadata {
                    instance_id: row.get(0)?,
                    schema_version: row.get(1)?,
                    parser_version: row.get(2)?,
                    revision: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(stored)
}

#[cfg(test)]
mod constraints;
#[cfg(test)]
mod query_plans;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod versions;
