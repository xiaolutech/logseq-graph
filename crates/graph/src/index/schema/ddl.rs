// The `depth` CHECK repeats the projection limit from `index::MAX_BLOCK_DEPTH`. It is a literal
// because SQLite cannot bind a constant into a table constraint; the schema tests keep the two in
// step.
pub(super) const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS graph (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    instance_id    TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    parser_version INTEGER NOT NULL,
    revision       INTEGER NOT NULL,
    config_signature TEXT
) STRICT;

-- `name` is the name as written, so a read can hand it back unchanged. `name_key` is the identity:
-- [`page_name_key`](crate::domain::page_name_key) of the same name, computed in Rust and stored, so
-- that SQL compares and indexes two names that differ only in case as one page. It is UNIQUE because
-- a read model that held `Ä` and `ä` as two pages would answer the same question two ways; discovery
-- already refuses a graph with such a pair, so a violation here is a caller bug that must not be
-- stored silently.
--
-- `relative_path` is the link between the two halves of one file. `pages` is the projection of a
-- file and `file_baseline` is the observation of that same file, written together in one
-- transaction, so the two were already paired one to one; what was missing was the column that says
-- so. Without it, answering which file holds this page meant discovering the graph again and
-- decoding every file name to rebuild the whole name-to-path map, which is the cost every
-- name-addressed write paid. NOT NULL is what makes it a rule of the database that a page row came
-- from an observed file, rather than a habit of the writer, and UNIQUE makes the one-to-one pairing
-- enforced rather than coincidental: one file holds one page, so a lookup cannot answer twice.
--
-- The foreign key deliberately states no `ON DELETE`, so the default NO ACTION applies. A baseline
-- is the observation of the file its page was projected from, so removing one while its page remains
-- would leave a row that cannot say where it came from. The scan's disappearance path already
-- removes the page first and the baseline second, and the key makes that the only order that works
-- instead of a rule the caller has to remember. CASCADE would be wrong in both directions: a
-- vanished baseline would silently take its page row with it, and that deletion would not advance
-- the revision, so the index would drop a page without recording a write.
--
-- The parent table is created below in the same batch: SQLite resolves a foreign key when a row is
-- written, not when the child table is declared.
--
-- `kind` says which half of the graph the page came from: a page named by its decoded file name, or a
-- journal named by its formatted date title. It duplicates nothing: the file baseline records only
-- what was observed about a file's bytes, and which directory that file is in is already part of
-- `relative_path`. Storing the kind here rather than deriving it on every read is what lets a
-- listing filter one kind out of the other without parsing a path per row, and the CHECK is what
-- makes a page row state a kind this build can read back, as a rule of the database instead of a
-- habit of the projection.
--
-- `journal_date` is the calendar day a journal is, and NULL for a regular page. The day is a fact
-- about the page, not about the file: it is what the journal's file name states under
-- `:journal/file-name-format`, and `name` is that same day rendered by `:journal/page-title-format`.
-- The rendering is not invertible, so a reader that needed the day could not recover it from the
-- name, and ordering journals by `name` orders a label (`Apr 19th, 2021` sorts before
-- `Apr 1st, 2021`). Storing the day is what makes a date range or a chronological listing a plain
-- comparison instead of a parse per row.
--
-- The spelling is fixed `YYYY-MM-DD`, which sorts lexicographically exactly as it sorts in time, so
-- SQL's `BETWEEN` and `ORDER BY` answer date questions on the text without a calendar function in
-- the query and without a second column. Ten characters is enough because `JournalDate::new`
-- refuses a year outside `0..=9999`, the range `yyyy` can write and read back.
--
-- The CHECK is the pairing, not a preference: a journal without a day would be a page whose
-- identity nothing can state, and a regular page with a day would be a day nothing names. Reading
-- either back would be a guess, so the database refuses to hold one.
CREATE TABLE IF NOT EXISTS pages (
    id            INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL UNIQUE,
    name_key      TEXT    NOT NULL UNIQUE,
    kind          TEXT    NOT NULL CHECK (kind IN ('page', 'journal')),
    journal_date  TEXT,
    relative_path TEXT    NOT NULL UNIQUE REFERENCES file_baseline (relative_path),
    block_count   INTEGER NOT NULL,
    revision      INTEGER NOT NULL,
    CHECK ((kind = 'journal') = (journal_date IS NOT NULL))
) STRICT;

CREATE TABLE IF NOT EXISTS page_claims (
    page_id  INTEGER NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    name     TEXT NOT NULL,
    name_key TEXT NOT NULL,
    kind     TEXT NOT NULL CHECK (kind IN ('entity', 'ref', 'tag', 'alias')),
    PRIMARY KEY (page_id, name_key, kind)
) STRICT;
CREATE INDEX IF NOT EXISTS page_claims_by_name ON page_claims (name_key, kind);

CREATE VIEW IF NOT EXISTS page_entities AS
SELECT id, name, name_key, kind, journal_date, relative_path, block_count, revision FROM pages
UNION ALL
SELECT NULL, MIN(claim.name), claim.name_key, 'page', NULL, NULL, 0, MAX(owner.revision)
  FROM page_claims AS claim JOIN pages AS owner ON owner.id = claim.page_id
 WHERE NOT EXISTS (SELECT 1 FROM pages WHERE pages.name_key = claim.name_key)
 GROUP BY claim.name_key;

-- `text` is the block's own content from the same parse that produced the structure and the fact
-- rows: `document::facts` removes the marker and the alignment indentation and joins continuation
-- lines, so this is not a raw source line, a line number, or a source map (ADR-0002, ADR-0007). An
-- empty block stores the empty string, never NULL. It is derived state: the file can reconstruct it
-- at any time, and changing the parser version rebuilds it rather than migrating it.
--
-- `durable_id` is the `id::` UUID the block states, if it states one, normalized to lowercase by
-- `domain::BlockId` before it is stored. It is derived like `text`: the property line is still the
-- authority and a rebuild reconstructs the column from the same parse. NULL means the block states
-- no `id::`. The CHECK repeats the length half of `BlockId::parse`; SQLite cannot call Rust, so the
-- full 8-4-4-4-12 shape is enforced at projection time and this only keeps obviously wrong text out
-- of the column the durable-target lookup seeks.
--
-- `ephemeral_id` is the other half of that rule (ADR-0012): a random UUID the projection mints for a
-- block that states no `id::`, normalized to lowercase before it is stored, and NULL for a block
-- that states one. Every place a read or a write asks which block this is resolves through one of
-- these two columns and never through a position, so the pair is an identity, not a hint. A rebuild
-- mints new values, which is what makes an id from a deleted index name nothing.
--
-- `CHECK ((durable_id IS NULL) <> (ephemeral_id IS NULL))` is the invariant as a rule of the
-- database rather than a habit of the projection: a block with both would have two addresses that
-- could disagree, and a block with neither would have no address at all. Neither state is readable
-- back, so the database refuses to hold one.
--
-- `task_marker` and `priority` are the two facts the block's own text can lead with: the task word
-- of `domain::TaskMarker` and, right after it, the `[#A]`-`[#C]` group of `domain::Priority`. Both
-- are NULL when the block states none, which is the ordinary case rather than an error, and the
-- CHECKs repeat the two enums' spellings so a value this build cannot decode is refused instead of
-- stored. The decisions behind them are not repeated here: whether a word counts is
-- `TaskMarker::leading`, whether a group sits where a priority belongs is `Priority::leading_after`,
-- and a reader that seeks these columns answers what those two functions answered at projection
-- time. They are derived state like the rest of the row: the text is still the authority and a
-- rebuild recomputes them.
CREATE TABLE IF NOT EXISTS blocks (
    id            INTEGER PRIMARY KEY,
    page_id       INTEGER NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    parent_id     INTEGER          REFERENCES blocks (id) ON DELETE CASCADE,
    preorder      INTEGER NOT NULL,
    depth         INTEGER NOT NULL,
    sibling_index INTEGER NOT NULL,
    marker_kind   TEXT    NOT NULL,
    heading_level INTEGER,
    text          TEXT    NOT NULL,
    durable_id    TEXT             CHECK (durable_id IS NULL OR length(durable_id) = 36),
    ephemeral_id  TEXT             CHECK (ephemeral_id IS NULL OR length(ephemeral_id) = 36),
    task_marker   TEXT             CHECK (task_marker IS NULL
                                      OR task_marker IN ('TODO', 'DOING', 'DONE', 'NOW', 'LATER',
                                                         'WAITING', 'CANCELED')),
    priority      TEXT             CHECK (priority IS NULL OR priority IN ('A', 'B', 'C')),
    UNIQUE (page_id, preorder),
    CHECK (depth BETWEEN 0 AND 255),
    CHECK ((durable_id IS NULL) <> (ephemeral_id IS NULL)),
    CHECK (
        (marker_kind = 'bullet'  AND heading_level IS NULL)
        OR (marker_kind = 'heading'
            AND heading_level IS NOT NULL
            AND heading_level BETWEEN 1 AND 6)
    )
) STRICT;

CREATE TABLE IF NOT EXISTS file_baseline (
    relative_path TEXT    PRIMARY KEY,
    size          INTEGER NOT NULL,
    modified_ns   INTEGER,
    content_hash  INTEGER NOT NULL,
    CHECK (size >= 0)
) STRICT;

-- Properties keep the written key and value verbatim: a `key:: value` line states text, and what
-- that text means (a date, a comma-separated list, a `tags::` value that links) is decided by a
-- later layer that still has this row. `ordinal` is the source position among the owner's property
-- lines, so a repeated key keeps every line and last-one-wins stays a query, not a write.
CREATE TABLE IF NOT EXISTS page_properties (
    page_id        INTEGER NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    ordinal        INTEGER NOT NULL,
    key            TEXT    NOT NULL,
    normalized_key TEXT    NOT NULL,
    value          TEXT    NOT NULL,
    PRIMARY KEY (page_id, ordinal),
    CHECK (ordinal >= 0)
) STRICT;

CREATE TABLE IF NOT EXISTS block_properties (
    block_id       INTEGER NOT NULL REFERENCES blocks (id) ON DELETE CASCADE,
    ordinal        INTEGER NOT NULL,
    key            TEXT    NOT NULL,
    normalized_key TEXT    NOT NULL,
    value          TEXT    NOT NULL,
    PRIMARY KEY (block_id, ordinal),
    CHECK (ordinal >= 0)
) STRICT;

-- One row per (block, referenced page). The backlinks question is which blocks reference this page,
-- so the index is on the target and the source is reached through `blocks`, whose row key is the
-- primary key. A target need not be a stored page: `[[new page]]` names a page that may have no file
-- yet, so a name is stored, not a page id.
--
-- `target_name` is the spelling the text used, `target_key` is `page_name_key` of it. Identity is the
-- key, so the primary key is (block, key) and a backlink join is `page_refs.target_key =
-- pages.name_key`, which is one indexed seek. SQLite's own `COLLATE NOCASE` is deliberately not used:
-- it folds ASCII only, so `[[Ä]]` would not match a page named `Ä`, while the rule every other layer
-- uses folds Unicode.
CREATE TABLE IF NOT EXISTS page_refs (
    block_id    INTEGER NOT NULL REFERENCES blocks (id) ON DELETE CASCADE,
    target_name TEXT    NOT NULL,
    target_key  TEXT    NOT NULL,
    PRIMARY KEY (block_id, target_key)
) STRICT;

-- Block references need no key column: the extraction already lowercases the UUID, and a UUID is
-- hexadecimal, so lowercase is its identity and a fold would be a no-op. The target is a block's
-- `id::` value rather than a row key: a reference may name a block that no file in this graph
-- contains.
CREATE TABLE IF NOT EXISTS block_refs (
    block_id    INTEGER NOT NULL REFERENCES blocks (id) ON DELETE CASCADE,
    target_uuid TEXT    NOT NULL,
    PRIMARY KEY (block_id, target_uuid)
) STRICT;

-- A tag names a page in Logseq, so it is identified by the same key as one: `#Tag` and `#tag` are one
-- tag, and `name_key` is what a tag page read will join on.
CREATE TABLE IF NOT EXISTS tags (
    block_id INTEGER NOT NULL REFERENCES blocks (id) ON DELETE CASCADE,
    name     TEXT    NOT NULL,
    name_key TEXT    NOT NULL,
    PRIMARY KEY (block_id, name_key)
) STRICT;

-- Reverse lookups: the rows that point at one target, without scanning every ref in the graph.
CREATE INDEX IF NOT EXISTS page_refs_by_target ON page_refs (target_key);
CREATE INDEX IF NOT EXISTS block_refs_by_target ON block_refs (target_uuid);
CREATE INDEX IF NOT EXISTS tags_by_name ON tags (name_key);

-- `blocks.parent_id` is a self-reference with `ON DELETE CASCADE`, so deleting one block makes
-- SQLite look up that block's children. Without this index the lookup is a full scan of `blocks`,
-- which is the largest table here, and the cost of replacing one page becomes
-- `blocks on the page x blocks in the graph`: a 141-block page in a 112852-block index spent over
-- two seconds per write in that cascade. No read queries `parent_id` today, so this index exists
-- for the foreign key action alone; a plan test pins it precisely because nothing else would
-- notice its removal.
--
-- It is deliberately a full index and not a partial one on `parent_id IS NOT NULL`: SQLite can only
-- use a partial index for a foreign key action when it can prove the predicate, and failing to
-- prove it would be a silent return to the full scan rather than an error.
CREATE INDEX IF NOT EXISTS blocks_by_parent ON blocks (parent_id);

-- The value of one property is the last row of one key, so the index ends with `ordinal` and the
-- read is a single ordered seek instead of a sort.
CREATE INDEX IF NOT EXISTS page_properties_by_key
    ON page_properties (page_id, normalized_key, ordinal);
CREATE INDEX IF NOT EXISTS block_properties_by_key
    ON block_properties (block_id, normalized_key, ordinal);
";

/// Index for the durable-id lookup, created only after the stored version has passed the check.
///
/// The statement names `blocks.durable_id`, which only this build's shape of `blocks` has, so it
/// cannot live in [`SCHEMA`]: there `init` would fail on an older database before it could report the
/// version mismatch that should refuse it.
///
/// Only a small minority of blocks state an `id::` (1342 of 113285 on the reference graph), so the
/// index is partial on `durable_id IS NOT NULL`: the other 99% add no entry at all instead of one
/// per block. A lookup is an equality on `durable_id`, which implies `IS NOT NULL`, so SQLite can
/// still prove the index's predicate and the seek is the same one — pinned by a plan test in
/// `index::query`, because a partial index the planner cannot prove is skipped silently, and the
/// read would fall back to scanning `blocks`.
pub(in crate::index) const DURABLE_ID_INDEX: &str = "\
CREATE INDEX IF NOT EXISTS blocks_by_durable_id ON blocks (durable_id) WHERE durable_id IS NOT NULL;
";

/// Index for the ephemeral-id lookup, created only after the stored version has passed the check.
///
/// It has the same shape as [`DURABLE_ID_INDEX`] and for the same reason: it names a column only
/// this build's `blocks` has, so it cannot live in [`SCHEMA`], where `init` would fail on an older
/// database before reporting the version mismatch that should refuse it.
///
/// It is `UNIQUE` where the durable index is not, and the asymmetry is the identities: two files can
/// state the same `id::`, because the file is the authority and deciding the person wrote an error
/// is not this layer's business, while an ephemeral id is minted here, so one value naming two
/// blocks is a defect this layer must not store. Rows with a NULL `ephemeral_id` never take part in
/// the index and never collide.
///
/// It stays a full index where the durable one became partial: `ephemeral_id` is NULL only for the
/// 1342 blocks that state an `id::`, so there is no sparse majority to skip and nothing to gain.
pub(in crate::index) const EPHEMERAL_ID_INDEX: &str = "\
CREATE UNIQUE INDEX IF NOT EXISTS blocks_by_ephemeral_id ON blocks (ephemeral_id);
";

/// The indexes the query surface seeks on, created only after the stored version has passed the
/// check.
///
/// The first two statements name `blocks.task_marker` and `blocks.priority`, columns only this
/// build's `blocks` has, so they cannot live in [`SCHEMA`] for the reason [`DURABLE_ID_INDEX`]
/// gives: there `init` would fail on an older database before it could report the version mismatch
/// that should refuse it. The other three name columns older databases have too and would create
/// against one without error; they are here anyway so every index this build reads through arrives
/// in one place, after the check, and no DDL of a refused database runs at all.
///
/// Each index is the direction one query reads. A task filter is one equality on a short column,
/// and a NULL `task_marker` or `priority` never matches an equality, so a block that is not a task
/// keeps no index entry at all: both are partial on `IS NOT NULL`, which is most of the table —
/// 15 `TODO`, 2 `DOING`, and 2937 `DONE` blocks out of 113285 on the reference graph state a
/// marker, and 179 state a priority. The two property indexes are the reverse of the `_by_key` pair
/// in [`SCHEMA`]: those answer "the properties of this owner", while a query that asks "which
/// owners state this property" seeks the name and reads the owner and ordinal out of the index.
/// The date index answers a journal listing: one kind of page ordered by its day, and only a
/// journal has a day (`journal_date` is `CHECK`ed non-NULL exactly for a journal), so it is partial
/// for the same reason.
///
/// Those partial predicates are only usable while the queries imply them. A leaf compiles to
/// `task_marker IN (?, ?)` or `priority IN (?)`, and SQLite treats an `IN` list as an equality for
/// exactly this purpose, which is why the SQL states no extra `IS NOT NULL` (a redundant one would
/// only make the generated query longer). The plan tests in `index::query_exec` pin that: if the
/// planner ever stops proving it, the leaf silently degrades from one seek to a full scan of
/// `blocks`, which is slower than the full index this replaced.
pub(in crate::index) const QUERY_INDEXES: &str = "\
CREATE VIRTUAL TABLE IF NOT EXISTS block_search USING fts5(
    text, content='', contentless_delete=1, tokenize='trigram case_sensitive 1'
);
CREATE VIRTUAL TABLE IF NOT EXISTS block_search_cjk USING fts5(
    text, content='', contentless_delete=1,
    tokenize='unicode61 remove_diacritics 0 categories ''L* M* N* P* S* Z* C*'' separators '' '''
);
CREATE TRIGGER IF NOT EXISTS block_search_insert AFTER INSERT ON blocks BEGIN
    INSERT INTO block_search(rowid, text) VALUES (new.id, search_key(new.text));
    INSERT INTO block_search_cjk(rowid, text) VALUES (new.id, search_cjk_tokens(new.text));
END;
CREATE TRIGGER IF NOT EXISTS block_search_delete AFTER DELETE ON blocks BEGIN
    DELETE FROM block_search WHERE rowid = old.id;
    DELETE FROM block_search_cjk WHERE rowid = old.id;
END;
CREATE TRIGGER IF NOT EXISTS block_search_update AFTER UPDATE OF text ON blocks BEGIN
    DELETE FROM block_search WHERE rowid = old.id;
    INSERT INTO block_search(rowid, text) VALUES (new.id, search_key(new.text));
    DELETE FROM block_search_cjk WHERE rowid = old.id;
    INSERT INTO block_search_cjk(rowid, text) VALUES (new.id, search_cjk_tokens(new.text));
END;
CREATE INDEX IF NOT EXISTS blocks_by_task_marker ON blocks (task_marker) WHERE task_marker IS NOT NULL;
CREATE INDEX IF NOT EXISTS blocks_by_priority ON blocks (priority) WHERE priority IS NOT NULL;
CREATE INDEX IF NOT EXISTS page_properties_by_name
    ON page_properties (normalized_key, page_id, ordinal);
CREATE INDEX IF NOT EXISTS block_properties_by_name
    ON block_properties (normalized_key, block_id, ordinal);
CREATE INDEX IF NOT EXISTS pages_by_journal_date ON pages (journal_date) WHERE journal_date IS NOT NULL;
";
