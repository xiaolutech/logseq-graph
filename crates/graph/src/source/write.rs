//! Atomic text writes.
//!
//! [`GraphSource::write_text`](crate::source::GraphSource::write_text) is the crate's only write
//! path. It replaces a file's bytes in one step: write the text to a temporary file in the target's
//! directory, fsync it, copy an existing target's permissions onto the temporary file, rename it
//! over the target, then fsync the parent directory.
//!
//! A reader therefore sees either the old bytes or the new bytes, never a mixture, and a crash
//! before the rename leaves the old file intact. When any step fails, the target keeps its previous
//! bytes and the temporary file is removed. The one failure after the point of no return is the
//! parent-directory fsync: the new bytes are already visible, only their survival across a crash is
//! unconfirmed, and that is reported as an error rather than hidden.
//!
//! This layer provides no locking, no write intents, and no index update after the write; those are
//! separate decisions in ADR-0004.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::discovery::{JOURNALS_DIR, PAGES_DIR};
use super::error::SourceError;
use super::file;

/// Directory, under the graph root, that receives a deleted page file.
///
/// Logseq parks a deleted page here instead of unlinking it, so the file is still the page's only
/// copy and a user can restore it by hand.
const RECYCLE_DIR: &str = "logseq/.recycle";

/// Name prefix of every temporary file this module creates.
///
/// The name is hidden and carries no page-file extension, so a leftover from a crashed process is
/// neither discovered as a page nor mistakable for the target file it was meant to become: it never
/// shares the target's name, and the next write always creates a fresh name instead of reusing one.
const TEMP_PREFIX: &str = ".logseq-graph-tmp-";

/// Serial that keeps temporary names unique when two writes land in the same nanosecond.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(0);

/// Replace the bytes of `path` with `text`, atomically.
///
/// `text` is written exactly as given: a byte order mark, original line endings, and a missing
/// final newline all survive, because `PageDocument::to_source` already produced them and this
/// layer converts nothing.
pub(crate) fn write_text(path: &Path, text: &str) -> Result<(), SourceError> {
    write_text_with_sync(path, text, sync_directory)
}

/// [`write_text`] with the directory-sync step supplied by the caller.
///
/// The engine's tests use this to fail only the step after the rename; production always passes
/// [`sync_directory`]. The rename itself stays real, so a test cannot accidentally describe a
/// different file state than the one the error names.
fn write_text_with_sync(
    path: &Path,
    text: &str,
    sync: impl FnOnce(Option<&Path>) -> Result<(), SourceError>,
) -> Result<(), SourceError> {
    let Some(parent) = path.parent() else {
        return Err(SourceError::Io {
            path: path.to_owned(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path has no parent directory",
            ),
        });
    };

    let temp_path = temp_path(parent);
    let outcome = commit(&temp_path, path, text, sync);
    if outcome.is_err() {
        // Every step that can fail does so with the temporary file still on disk. After the rename
        // succeeds the temporary name no longer exists, so this removal never erases a committed
        // write.
        drop(fs::remove_file(&temp_path));
    }
    outcome
}

/// Create `path` with `text`, refusing to replace an existing file.
///
/// Unlike [`write_text`] this never substitutes bytes for a file that is already there: the open is
/// `create_new`, so a file created by another process between the caller's check and this open is a
/// [`SourceError::AlreadyExists`] instead of a page losing its content. A missing parent directory is
/// created, which is what lets the first page of an empty graph be created; the parents this creates
/// are included in the directory fsync, so the new entry survives a crash.
///
/// The bytes are written exactly as given and are fsynced before the call returns, like
/// [`write_text`]. No temporary file is needed here: the file did not exist, so there is no previous
/// state to make atomic against.
pub(crate) fn create_text(path: &Path, text: &str) -> Result<(), SourceError> {
    create_text_with_sync(path, text, |file| file.sync_all(), sync_directory)
}

/// [`create_text`] with the file and directory sync steps supplied by the caller.
///
/// The production wrapper passes the real syncs. The test-only entry point can fail either one while
/// keeping the `create_new` open and the cleanup real, so the reported state is derived from what is
/// actually on disk.
fn create_text_with_sync(
    path: &Path,
    text: &str,
    sync_file: impl FnOnce(&fs::File) -> io::Result<()>,
    sync_dir: impl FnOnce(Option<&Path>) -> Result<(), SourceError>,
) -> Result<(), SourceError> {
    let Some(parent) = path.parent() else {
        return Err(SourceError::Io {
            path: path.to_owned(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path has no parent directory",
            ),
        });
    };

    fs::create_dir_all(parent).map_err(|source| SourceError::Io {
        path: parent.to_owned(),
        source,
    })?;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| match source.kind() {
            io::ErrorKind::AlreadyExists => SourceError::AlreadyExists {
                path: path.to_owned(),
            },
            _ => SourceError::Io {
                path: path.to_owned(),
                source,
            },
        })?;

    let result = (|| -> io::Result<()> {
        file.write_all(text.as_bytes())?;
        sync_file(&file)
    })();
    if let Err(source) = result {
        if let Err(cleanup) = fs::remove_file(path) {
            return Err(SourceError::Committed {
                path: path.to_owned(),
                step: path.to_owned(),
                source: io::Error::new(
                    cleanup.kind(),
                    format!(
                        "the create failed ({source}) and the partial file could not be removed: \
                         {cleanup}"
                    ),
                ),
            });
        }
        return Err(SourceError::Io {
            path: path.to_owned(),
            source,
        });
    }

    sync_dir(Some(parent)).map_err(|error| match error {
        SourceError::Io { path: step, source } => SourceError::Committed {
            path: path.to_owned(),
            step,
            source,
        },
        other => other,
    })
}

/// Move a page file into the graph's recycle bin, answering where it landed.
///
/// `relative_path` is a graph-relative page path: its first component is `pages` or `journals`, and
/// the file keeps its own name behind that directory's name as a prefix, so `pages/Some Note.md`
/// lands as `logseq/.recycle/pages_Some Note.md` and a journal cannot collide with a page of the
/// same file name. A file that is already in the bin is not overwritten: the second one takes a
/// millisecond stamp, `pages_Some Note.<millis>.md`.
///
/// The move is a single [`fs::rename`] inside one file system, not a copy: a rename is atomic, while
/// a copy that fails halfway would leave the page in two places and one of them short. The recycle
/// directory is created when the graph has none, and both the directory it was moved out of and the
/// one it moved into are fsynced, so the move survives a crash.
///
/// A path whose first component is neither `pages` nor `journals` is
/// [`SourceError::InvalidPageName`] rather than a file this layer invents a prefix for.
pub(crate) fn move_to_recycle(
    graph_root: &Path,
    relative_path: &Path,
) -> Result<PathBuf, SourceError> {
    let invalid = |reason: String| SourceError::InvalidPageName {
        path: graph_root.join(relative_path),
        reason,
    };

    let mut components = relative_path.components();
    let dir = match components.next() {
        Some(Component::Normal(name)) if name.to_str() == Some(PAGES_DIR) => PAGES_DIR,
        Some(Component::Normal(name)) if name.to_str() == Some(JOURNALS_DIR) => JOURNALS_DIR,
        _ => {
            return Err(invalid(format!(
                "only files under `{PAGES_DIR}` or `{JOURNALS_DIR}` can be recycled"
            )))
        }
    };
    if !components.all(|component| matches!(component, Component::Normal(_))) {
        return Err(invalid(
            "the path must be a plain graph-relative path".to_owned(),
        ));
    }
    let Some(file_name) = relative_path.file_name() else {
        return Err(invalid("the path names no file".to_owned()));
    };
    let file_name = file_name.to_string_lossy();

    let source_path = graph_root.join(relative_path);
    let parent = file::directory_within(
        graph_root,
        source_path
            .parent()
            .ok_or_else(|| invalid("the path names no file".to_owned()))?,
    )?;
    let source_path = parent.join(
        relative_path
            .file_name()
            .ok_or_else(|| invalid("the path names no file".to_owned()))?,
    );
    let recycle = file::directory_within(graph_root, &graph_root.join(RECYCLE_DIR))?;
    fs::create_dir_all(&recycle).map_err(|source| SourceError::Io {
        path: recycle.clone(),
        source,
    })?;

    let stem = file_name.strip_suffix(".md").unwrap_or(&file_name);
    let mut target = recycle.join(format!("{dir}_{file_name}"));
    if target.symlink_metadata().is_ok() {
        // The clock is the tie-breaker; adding to it instead of retrying is only for a frozen clock
        // that hands back a name already in the bin.
        let mut millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        loop {
            target = recycle.join(format!("{dir}_{stem}.{millis}.md"));
            if target.symlink_metadata().is_err() {
                break;
            }
            millis += 1;
        }
    }

    fs::rename(&source_path, &target).map_err(|source| SourceError::Io {
        path: source_path.clone(),
        source,
    })?;
    let target_in_graph = graph_root.join(relative_path);
    sync_directory(Some(&recycle)).map_err(|error| after_commit(&target, error))?;
    sync_directory(
        relative_path
            .parent()
            .map(|parent| graph_root.join(parent))
            .as_deref(),
    )
    .map_err(|error| after_commit(&target_in_graph, error))?;
    Ok(target)
}

/// Remove the file at `path`.
///
/// The engine calls this to undo a file it created within one call after the index refused to
/// project it: the file did not exist before (`create_new` proved that), so removing it restores
/// the graph to what it was instead of leaving a page file no row describes. The caller decides
/// whether a failed removal matters, so the outcome is returned rather than swallowed here.
pub(crate) fn remove_text(path: &Path) -> Result<(), SourceError> {
    fs::remove_file(path).map_err(|source| SourceError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Write, sync, and rename, leaving cleanup to the caller on failure.
fn commit(
    temp_path: &Path,
    target: &Path,
    text: &str,
    sync: impl FnOnce(Option<&Path>) -> Result<(), SourceError>,
) -> Result<(), SourceError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .map_err(|source| SourceError::Io {
            path: temp_path.to_owned(),
            source,
        })?;

    let result = (|| -> io::Result<()> {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    })();
    if let Err(source) = result {
        return Err(SourceError::Io {
            path: temp_path.to_owned(),
            source,
        });
    }

    preserve_mode(target, temp_path)?;

    if let Err(source) = fs::rename(temp_path, target) {
        return Err(SourceError::Io {
            path: target.to_owned(),
            source,
        });
    }

    sync(target.parent()).map_err(|error| match error {
        SourceError::Io { path, source } => SourceError::Committed {
            path: target.to_owned(),
            step: path,
            source,
        },
        other => other,
    })
}

/// Copy the target's permissions onto the temporary file, so replacing a file keeps its mode.
///
/// A missing target keeps the platform default for a newly created file. A stat that fails for any
/// other reason fails the write, because guessing a mode could silently loosen or tighten an
/// existing file's access. On Windows the only preserved bit is the read-only flag; Unix preserves
/// the full mode.
fn preserve_mode(target: &Path, temp_path: &Path) -> Result<(), SourceError> {
    match fs::metadata(target) {
        Ok(metadata) => {
            let permissions = metadata.permissions();
            fs::set_permissions(temp_path, permissions).map_err(|source| SourceError::Io {
                path: temp_path.to_owned(),
                source,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SourceError::Io {
            path: target.to_owned(),
            source,
        }),
    }
}

/// fsync the target's parent directory so the rename itself survives a crash.
///
/// Only Unix lets a process open a directory and sync it. Windows has no equivalent in `std`, so
/// there the rename reaches disk on the operating system's own schedule; this is a weaker,
/// documented guarantee rather than a simulated one.
#[cfg(unix)]
pub(super) fn sync_directory(parent: Option<&Path>) -> Result<(), SourceError> {
    let Some(parent) = parent else {
        return Ok(());
    };
    let file = fs::File::open(parent).map_err(|source| SourceError::Io {
        path: parent.to_owned(),
        source,
    })?;
    file.sync_all().map_err(|source| SourceError::Io {
        path: parent.to_owned(),
        source,
    })
}

#[cfg(not(unix))]
fn sync_directory(_parent: Option<&Path>) -> Result<(), SourceError> {
    Ok(())
}

/// Mark a failed durability step after the target already changed.
pub(crate) fn after_commit(target: &Path, error: SourceError) -> SourceError {
    match error {
        SourceError::Io { path, source } => SourceError::Committed {
            path: target.to_owned(),
            step: path,
            source,
        },
        other => other,
    }
}

/// Build a unique temporary path inside `parent`.
///
/// Uniqueness comes from the process id, the nanosecond clock, and a process-wide serial. The
/// `create_new` open turns a collision into an error rather than a silent overwrite.
fn temp_path(parent: &Path) -> PathBuf {
    let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(
        "{TEMP_PREFIX}{}-{nanos}-{serial}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph root that exists for the duration of one test.
    fn graph_root(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "logseq-graph-recycle-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create graph root");
        // `move_to_recycle` answers canonical paths (TMPDIR may be a symlink, as on macOS).
        fs::canonicalize(&root).expect("canonicalize graph root")
    }

    #[test]
    fn the_recycled_name_carries_the_directory_it_came_from() {
        let root = graph_root("prefix");
        fs::create_dir_all(root.join("pages")).expect("create pages");
        fs::write(root.join("pages/Some Note.md"), b"- one\n").expect("write page");

        let recycled = move_to_recycle(&root, Path::new("pages/Some Note.md")).expect("recycle");

        assert_eq!(recycled, root.join("logseq/.recycle/pages_Some Note.md"));
        assert_eq!(
            fs::read(&recycled).expect("read recycled"),
            b"- one\n",
            "the move preserves the bytes"
        );
        assert!(!root.join("pages/Some Note.md").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rename_whose_directory_sync_fails_still_reports_the_new_bytes() {
        let root = graph_root("syndir");
        fs::create_dir_all(root.join("pages")).expect("create pages");
        let target = root.join("pages/Page.md");
        fs::write(&target, b"- old\n").expect("write page");

        let error = write_text_with_sync(&target, "- new\n", |_| {
            Err(SourceError::Io {
                path: target.parent().expect("page has a parent").to_owned(),
                source: io::Error::other("injected directory sync failure"),
            })
        })
        .expect_err("the sync seam fails after the rename");

        assert!(
            matches!(error, SourceError::Committed { .. }),
            "expected Committed, got {error:?}"
        );
        assert_eq!(
            fs::read(&target).expect("read target"),
            b"- new\n",
            "the rename already made the new bytes visible"
        );
        let residues: Vec<_> = fs::read_dir(root.join("pages"))
            .expect("list pages")
            .filter_map(|entry| {
                let name = entry.expect("entry").file_name();
                name.to_string_lossy()
                    .starts_with(TEMP_PREFIX)
                    .then_some(name)
            })
            .collect();
        assert!(residues.is_empty(), "the temporary name is gone");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_create_whose_bytes_cannot_be_synced_leaves_no_target() {
        let root = graph_root("create-sync");
        fs::create_dir_all(root.join("pages")).expect("create pages");
        let target = root.join("pages/Page.md");
        let error = create_text_with_sync(
            &target,
            "- new\n",
            |_| Err(io::Error::other("injected file sync failure")),
            |_| Ok(()),
        )
        .expect_err("the injected create sync fails");

        assert!(
            matches!(error, SourceError::Io { .. }),
            "a create that removed its failed file is still unwritten: {error:?}"
        );
        assert!(!target.exists(), "the failed create leaves no page file");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_create_whose_directory_sync_fails_names_the_target_and_the_step() {
        let root = graph_root("create-syndir");
        fs::create_dir_all(root.join("pages")).expect("create pages");
        let target = root.join("pages/Page.md");
        let parent = target.parent().expect("page has a parent").to_owned();
        let error = create_text_with_sync(
            &target,
            "- new\n",
            |_| Ok(()),
            move |_| {
                Err(SourceError::Io {
                    path: parent,
                    source: io::Error::other("injected directory sync failure"),
                })
            },
        )
        .expect_err("the injected directory sync fails after the create");

        match error {
            SourceError::Committed { path, step, source } => {
                assert_eq!(path, target, "the changed file is the created target");
                assert_eq!(
                    step,
                    target.parent().expect("page has a parent"),
                    "the failed step is the parent directory"
                );
                assert_eq!(source.to_string(), "injected directory sync failure");
            }
            other => panic!("expected Committed, got {other:?}"),
        }
        assert_eq!(
            fs::read(&target).expect("read created file"),
            b"- new\n",
            "the create already made the new bytes visible"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_journal_file_is_prefixed_with_journals_not_pages() {
        let root = graph_root("journal");
        fs::create_dir_all(root.join("journals")).expect("create journals");
        fs::write(root.join("journals/2024-01-02.md"), b"- entry\n").expect("write journal");

        let recycled =
            move_to_recycle(&root, Path::new("journals/2024-01-02.md")).expect("recycle");

        assert_eq!(
            recycled,
            root.join("logseq/.recycle/journals_2024-01-02.md")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_path_outside_pages_and_journals_is_refused_before_anything_moves() {
        let root = graph_root("refused");
        fs::create_dir_all(root.join("logseq")).expect("create logseq");
        fs::write(root.join("logseq/config.edn"), b"{}").expect("write config");

        let error = move_to_recycle(&root, Path::new("logseq/config.edn"))
            .expect_err("a configuration file is not a page");

        assert!(
            matches!(error, SourceError::InvalidPageName { .. }),
            "expected InvalidPageName, got {error:?}"
        );
        assert!(
            root.join("logseq/config.edn").exists(),
            "the refused file stays where it was"
        );
        assert!(
            !root.join("logseq/.recycle").exists(),
            "a refusal creates no recycle directory"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
