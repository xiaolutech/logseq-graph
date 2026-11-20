use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::functions::{Context as SqlContext, FunctionFlags};
use rusqlite::types::{ToSqlOutput, Value as SqlValue, ValueRef};
use rusqlite::{Connection, OptionalExtension};

use super::{schema, storage, Index, IndexError, MoveAnchorKind, MoveIntent};
use crate::domain::{search_key, search_key_char, MoveBlockAddress};

/// How long a connection waits for another writer before reporting a busy database.
///
/// A page replacement is one short transaction, so a second connection in this process, or another
/// process projecting the same graph, should wait for it instead of failing at once with
/// `SQLITE_BUSY`. Ten seconds is far above any single page write yet still bounded, so a writer that
/// never finishes surfaces as an error instead of a hang. The value is set explicitly rather than
/// inherited from the driver's default, which the driver's own documentation calls subject to
/// change.
const BUSY_TIMEOUT: Duration = Duration::from_millis(10_000);

impl Index {
    pub(crate) fn reader(&self) -> Result<Option<Self>, IndexError> {
        self.location
            .as_ref()
            .map(|location| Self::open_reader(&location.logical))
            .transpose()
    }

    pub(super) fn open_reader(path: &Path) -> Result<Self, IndexError> {
        let location = storage::Location::open(path)?;
        let conn = Connection::open_with_flags(
            &location.actual,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        register_functions(&conn)?;
        Ok(Self {
            conn,
            location: Some(location),
            read_only: true,
            #[cfg(test)]
            fail_next_answer: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            fail_next_metadata: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Open an empty in-memory index and initialize its schema.
    pub fn open_in_memory() -> Result<Self, IndexError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    /// Open the index stored at `path`, creating it when it is missing.
    ///
    /// A database written by an incompatible build, meaning another schema or parser version, is
    /// deleted and created again empty. The caller then rebuilds the derived state from the files
    /// (ADR-0002). The new database gets a new instance id, so no result from the discarded one can
    /// be mistaken for a result of this one.
    ///
    /// A database that carries another application's `application_id` is reported as
    /// [`IndexError::NotAnIndex`] and left exactly as it is: it is not this project's derived state,
    /// so neither writing tables into it nor deleting it is safe.
    ///
    /// Any other failure is returned as is: a file that is unreadable or not a database is a real
    /// problem, and deleting it would hide the cause instead of repairing derived state.
    pub fn open_at(path: impl AsRef<Path>) -> Result<Self, IndexError> {
        let location = storage::Location::open(path.as_ref())?;
        let path = &location.actual;
        let mut index = match Self::open_at_once(path) {
            Err(IndexError::Incompatible { .. }) => {
                if Self::legacy_intent_at(path)?.is_some() {
                    return Err(IndexError::CorruptRow {
                        column: "write_intent", value: "pending legacy move must be migrated through Graph::open before rebuilding".to_owned(),
                    });
                }
                discard(path)?;
                Self::open_at_once(path)
            }
            opened => opened,
        }?;
        index.location = Some(location);
        Ok(index)
    }

    pub(super) fn open_at_once(path: &Path) -> Result<Self, IndexError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| IndexError::Io {
                    path: parent.to_owned(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(path)?;

        // Set before the first statement that can block, and before anything writes to the file.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // The file marker comes first: a database of another application must be refused before any
        // statement below changes it, including the journal-mode switch that rewrites its header.
        claim_application_id(&conn, path)?;

        // WAL lets a later reader keep reading while a page replacement commits. The database lives
        // outside the graph in the cache directory, so the extra `-wal` and `-shm` files are not
        // graph content. A filesystem that cannot host WAL keeps the rollback journal: slower, not
        // wrong, so the settled mode is not an error.
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        // The index is derived state that is rebuilt from the files after any loss, so a commit
        // does not need to survive power loss; NORMAL keeps it durable across a process crash and
        // saves an fsync per page replacement.
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        Self::prepare(conn)
    }

    pub(crate) fn legacy_intent_at(path: &Path) -> Result<Option<MoveIntent>, IndexError> {
        let actual = storage::resolve(path)?;
        let path = actual.as_path();
        if !path.try_exists().map_err(|source| IndexError::Io {
            path: path.to_owned(),
            source,
        })? {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let app: i32 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
        if app != 0 && app != schema::APPLICATION_ID {
            return Err(IndexError::NotAnIndex {
                path: path.to_owned(),
                found: i64::from(app),
            });
        }
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'write_intent')", [], |row| row.get(0))?;
        if exists {
            read_intent(&conn)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn clear_legacy_intent_at(path: &Path) -> Result<(), IndexError> {
        let actual = storage::resolve(path)?;
        let path = actual.as_path();
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute("DELETE FROM write_intent", [])?;
        Ok(())
    }

    /// Apply the settings every connection needs and create or verify the schema.
    ///
    /// The busy timeout is not set here: only a file-backed index can be opened by a second
    /// connection or another process, so [`Index::open_at`] applies it before this runs.
    fn prepare(conn: Connection) -> Result<Self, IndexError> {
        // The bundled SQLite build enables foreign keys by default, but the page cascade that
        // removes replaced rows must not depend on a compile-time default. The setting is per
        // connection and is not stored in the file, so every open repeats it.
        conn.pragma_update(None, "foreign_keys", true)?;
        register_functions(&conn)?;
        schema::init(&conn)?;
        Ok(Self {
            conn,
            location: None,
            read_only: false,
            #[cfg(test)]
            fail_next_answer: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            fail_next_metadata: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
}

/// Read the pending [`MoveIntent`], or `None` when the table is empty.
fn read_intent(conn: &Connection) -> Result<Option<MoveIntent>, IndexError> {
    let row = conn
        .query_row(
            "SELECT moved_block_id, anchor_block_id, anchor_kind, from_relative_path, from_page,
                    from_pre_hash, from_post_hash, to_relative_path, to_page, to_pre_hash,
                    to_post_hash
               FROM write_intent WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )
        .optional()?;

    let Some(row) = row else {
        return Ok(None);
    };
    let parse_id = |column: &'static str, value: &String| {
        value.parse().map_err(|_| IndexError::CorruptRow {
            column,
            value: value.clone(),
        })
    };
    Ok(Some(MoveIntent {
        moved: MoveBlockAddress::Durable(parse_id("write_intent.moved_block_id", &row.0)?),
        anchor: MoveBlockAddress::Durable(parse_id("write_intent.anchor_block_id", &row.1)?),
        anchor_kind: MoveAnchorKind::from_str(&row.2).ok_or_else(|| IndexError::CorruptRow {
            column: "write_intent.anchor_kind",
            value: row.2.clone(),
        })?,
        from_relative_path: PathBuf::from(row.3),
        from_page: row.4,
        from_pre_hash: row.5 as u64,
        from_post_hash: row.6 as u64,
        to_relative_path: PathBuf::from(row.7),
        to_page: row.8,
        to_pre_hash: row.9 as u64,
        to_post_hash: row.10 as u64,
    }))
}

/// Register the SQL scalar function behind the search fold.
///
fn register_functions(conn: &Connection) -> Result<(), IndexError> {
    register_search_key(conn)?;
    super::query_exec::register_functions(conn)?;
    conn.create_scalar_function(
        "search_cjk_tokens",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(ctx
                .get::<Option<String>>(0)?
                .map(|text| super::cjk_search::tokens(&text)))
        },
    )?;
    Ok(())
}

/// [`search_key`](crate::domain::search_key) is the one rule; this exposes it to SQL so the text
/// reads can fold a stored text whose characters a needle's fold does not touch, which built-in
/// `lower` cannot answer. It returns its argument untouched when the fold changes nothing, so the
/// common ASCII case costs no copy; SQLite then compares the stored bytes directly.
///
/// The function is per connection, like every pragma and collation this module installs, so every
/// open repeats the registration; the statements in `query.rs` only name it.
fn register_search_key(conn: &Connection) -> Result<(), IndexError> {
    conn.create_scalar_function(
        "search_key",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx: &mut SqlContext<'_>| -> rusqlite::Result<ToSqlOutput<'static>> {
            let raw = ctx.get_raw(0);
            if raw == ValueRef::Null {
                return Ok(ToSqlOutput::Owned(SqlValue::Null));
            }
            let text = raw.as_str()?;
            if text.chars().all(|c| search_key_char(c) == c) {
                // Unchanged under the fold: hand back the argument itself.
                return Ok(ToSqlOutput::Arg(0));
            }
            Ok(ToSqlOutput::Owned(SqlValue::Text(search_key(text))))
        },
    )?;
    Ok(())
}

/// Mark this database file as this project's index, or refuse a file that belongs to another one.
///
/// The marker lives in the file header, so it identifies the file rather than a version of the
/// schema: it survives the delete-and-rebuild that an incompatible version triggers. A value of
/// zero means a database created before this marker existed, or one made by hand, so it is claimed
/// in place; claiming marks the file and recreates nothing. Any other value belongs to another
/// application, and the caller must neither write to that file nor delete it.
fn claim_application_id(conn: &Connection, path: &Path) -> Result<(), IndexError> {
    let found: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if found == i64::from(schema::APPLICATION_ID) {
        return Ok(());
    }
    if found != 0 {
        return Err(IndexError::NotAnIndex {
            path: path.to_owned(),
            found,
        });
    }
    conn.pragma_update(None, "application_id", schema::APPLICATION_ID)?;
    Ok(())
}

/// Delete an index file and the sidecar files SQLite keeps next to it.
///
/// Removing only the main file would leave a write-ahead log that a freshly created database must
/// ignore, and a rollback journal from the filesystem that cannot host WAL. Removing every sidecar
/// leaves nothing to interpret, whichever journal mode the file settled in.
fn discard(path: &Path) -> Result<(), IndexError> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let target = match suffix {
            "" => path.to_owned(),
            suffix => {
                let mut name = path.as_os_str().to_owned();
                name.push(suffix);
                PathBuf::from(name)
            }
        };
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(IndexError::Io {
                    path: target,
                    source,
                })
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_support::*;

    #[test]
    fn readers_stay_read_only_when_adopting_a_published_generation() {
        let dir = TempDir::new("read-only-generation");
        let mut writer = Index::open_at(dir.file()).expect("writer");
        project(&mut writer, "alpha", "- before\n").expect("project");
        let mut reader = writer.reader().expect("reader").expect("disk");
        let old = reader.storage_path().expect("path").to_owned();
        let mut shadow = writer.shadow().expect("shadow");
        project(
            shadow.database().expect("shadow index"),
            "alpha",
            "- after\n",
        )
        .expect("project shadow");
        writer = shadow.publish().expect("publish");
        assert_eq!(reader.storage_path(), Some(old.as_path()));
        assert!(reader.reopen_if_published().expect("adopt"));
        assert_eq!(reader.storage_path(), writer.storage_path());
        let error = reader
            .conn
            .execute("UPDATE graph SET revision = revision + 1", [])
            .expect_err("read-only connection");
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(error, _) if error.code == rusqlite::ErrorCode::ReadOnly)
        );
        let folded: String = reader
            .conn
            .query_row("SELECT search_key('ÄRGER')", [], |row| row.get(0))
            .expect("function on reopened reader");
        assert_eq!(folded, "ärger");
        let tokens: String = reader
            .conn
            .query_row("SELECT search_cjk_tokens('中文')", [], |row| row.get(0))
            .expect("CJK function on reopened reader");
        assert_eq!(tokens, "u中 b中文 u文 ");
        let property: bool = reader
            .conn
            .query_row(
                "SELECT query_property_matches('Book', 'kind', 'book')",
                [],
                |row| row.get(0),
            )
            .expect("property function on reopened reader");
        assert!(property);
    }

    #[test]
    fn foreign_keys_are_enforced_on_the_production_connection() {
        let index = Index::open_in_memory().expect("open index");

        let error = index
            .conn
            .execute(
                "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                     marker_kind, heading_level, text)
                 VALUES (999, NULL, 0, 0, 0, 'bullet', NULL, '')",
                [],
            )
            .expect_err("a block without its page must be rejected");

        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(error, _)
                if error.code == rusqlite::ErrorCode::ConstraintViolation
        ));
    }

    #[test]
    fn an_on_disk_index_keeps_its_instance_and_rows_across_opens() {
        let dir = TempDir::new("reopen");
        let path = dir.file();

        let first = {
            let mut index = Index::open_at(&path).expect("open on-disk index");
            project(&mut index, "page", "- a\n").expect("replace page");
            index.metadata().expect("read metadata")
        };
        assert_eq!(first.revision(), 1);

        let reopened = Index::open_at(&path).expect("reopen on-disk index");
        let metadata = reopened.metadata().expect("read metadata");
        assert_eq!(metadata.instance_id(), first.instance_id());
        assert_eq!(metadata.revision(), 1);
        assert!(reopened.page_tree("page").expect("query").is_some());
    }

    #[test]
    fn an_incompatible_database_file_is_deleted_and_recreated() {
        let dir = TempDir::new("incompatible");
        let path = dir.file();
        std::fs::create_dir_all(path.parent().expect("file has a parent"))
            .expect("create index directory");

        // Only the version marker matters here: `open_at` must reject the database before it can
        // read anything else, whatever else the old file contains.
        let old = Connection::open(&path).expect("open old database");
        old.execute_batch(
            "CREATE TABLE graph (
                 id             INTEGER PRIMARY KEY CHECK (id = 1),
                 instance_id    TEXT    NOT NULL,
                 schema_version INTEGER NOT NULL,
                 parser_version INTEGER NOT NULL,
                 revision       INTEGER NOT NULL
             ) STRICT;",
        )
        .expect("create old metadata table");
        old.execute(
            "INSERT INTO graph (id, instance_id, schema_version, parser_version, revision)
             VALUES (1, 'old-instance', 1, 1, 42)",
            [],
        )
        .expect("record old version");
        drop(old);

        let rebuilt = Index::open_at(&path).expect("an old database is rebuilt, not reported");
        let metadata = rebuilt.metadata().expect("read metadata");
        assert_ne!(metadata.instance_id(), "old-instance");
        assert_eq!(metadata.revision(), 0, "nothing was projected yet");
    }

    #[test]
    fn an_on_disk_index_runs_in_wal_mode_with_foreign_keys_enforced() {
        // The settings are per connection and are not stored in the file, so every open must apply
        // them again. This pins that they are actually applied, not just written down: the local
        // temporary directory hosts a WAL, so a downgrade here is a regression rather than an
        // environment limitation.
        let dir = TempDir::new("pragmas");
        let index = Index::open_at(dir.file()).expect("open on-disk index");

        let journal_mode: String = index
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read journal mode");
        assert_eq!(journal_mode, "wal");

        let foreign_keys: bool = index
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read foreign_keys");
        assert!(foreign_keys);
    }

    #[test]
    fn a_second_connection_waits_for_a_writer_instead_of_failing_at_once() {
        // Without a busy timeout the driver reports `SQLITE_BUSY` on the first contended write, so a
        // second connection in this process, or another process projecting the same graph, would
        // fail a page replacement instead of waiting for the one in flight. The assertion is only
        // able to fail because the value differs from the driver's own default.
        let dir = TempDir::new("busy-timeout");
        let index = Index::open_at(dir.file()).expect("open on-disk index");

        let timeout: i64 = index
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("read busy timeout");
        assert_eq!(timeout, BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn an_index_of_this_project_opens_under_its_own_application_id() {
        let dir = TempDir::new("application-id");
        let path = dir.file();

        let instance = {
            let index = Index::open_at(&path).expect("create the index");
            assert_eq!(
                application_id(&index.conn),
                i64::from(schema::APPLICATION_ID),
                "a created index must be marked as this project's"
            );
            index
                .metadata()
                .expect("read metadata")
                .instance_id()
                .to_owned()
        };

        let reopened = Index::open_at(&path).expect("reopen the index");
        assert_eq!(
            application_id(&reopened.conn),
            i64::from(schema::APPLICATION_ID),
            "the marker must survive in the file, not just in one connection"
        );
        assert_eq!(
            reopened.metadata().expect("read metadata").instance_id(),
            instance,
            "a marked index is reused, not rebuilt"
        );
    }

    #[test]
    fn a_database_of_another_application_is_refused_and_left_in_place() {
        const FOREIGN: i64 = 0x1234_5678;
        let dir = TempDir::new("foreign-database");
        let path = dir.file();
        std::fs::create_dir_all(path.parent().expect("file has a parent"))
            .expect("create index directory");
        {
            let foreign = Connection::open(&path).expect("open a database of another application");
            foreign
                .pragma_update(None, "application_id", FOREIGN)
                .expect("mark it as another application's");
            foreign
                .execute_batch("CREATE TABLE foreign_data (id INTEGER PRIMARY KEY) STRICT;")
                .expect("store another application's table");
        }

        let error = Index::open_at(&path).expect_err("another application's database is refused");
        match error {
            IndexError::NotAnIndex {
                path: refused,
                found,
            } => {
                assert_eq!(refused, path);
                assert_eq!(found, FOREIGN);
            }
            other => panic!("expected a foreign database to be refused, got {other:?}"),
        }

        assert!(path.is_file(), "a foreign database must never be deleted");
        let foreign = Connection::open(&path).expect("reopen the foreign database");
        assert_eq!(
            application_id(&foreign),
            FOREIGN,
            "its marker must be untouched"
        );
        let tables = table_names(&foreign);
        assert_eq!(
            tables,
            ["foreign_data"],
            "no table of this project may be created in it"
        );
    }

    #[test]
    fn a_database_without_an_application_id_is_claimed_in_place() {
        let dir = TempDir::new("unmarked-database");
        let path = dir.file();
        std::fs::create_dir_all(path.parent().expect("file has a parent"))
            .expect("create index directory");
        {
            let unmarked = Connection::open(&path).expect("open an unmarked database");
            assert_eq!(application_id(&unmarked), 0, "the file must start unmarked");
            unmarked
                .execute_batch("CREATE TABLE keep_me (id INTEGER PRIMARY KEY) STRICT;")
                .expect("store a row that a rebuild would lose");
        }

        let index = Index::open_at(&path).expect("an unmarked database is claimed");

        assert_eq!(
            application_id(&index.conn),
            i64::from(schema::APPLICATION_ID),
            "claiming must record the marker"
        );
        // Claiming marks the existing file; it does not delete and recreate it.
        let kept: i64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM keep_me", [], |row| row.get(0))
            .expect("the pre-existing table survives");
        assert_eq!(kept, 0);
        index.metadata().expect("the schema is initialized");
    }

    #[test]
    fn discarding_an_index_removes_every_sidecar_file() {
        // A filesystem that cannot host WAL keeps a rollback journal, so `-journal` is a real
        // leftover next to the files this test can create on any platform.
        let dir = TempDir::new("discard");
        let path = dir.file();
        std::fs::create_dir_all(path.parent().expect("file has a parent"))
            .expect("create index directory");

        let mut sidecars = Vec::new();
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            let target = PathBuf::from(name);
            std::fs::write(&target, b"sidecar").expect("write sidecar");
            sidecars.push(target);
        }

        discard(&path).expect("discard the index");

        for sidecar in sidecars {
            assert!(
                !sidecar.exists(),
                "{} must be removed with the index",
                sidecar.display()
            );
        }
    }
}
