//! Graph files: configuration, discovery, and text reads.
//!
//! This is the only layer that touches the graph filesystem (ADR-0003). `document` stays a pure
//! parser and `index` stays a SQLite projection, so both remain storage-agnostic.
//!
//! Scope: opening a graph root, reading the configuration that affects Markdown discovery,
//! discovering Markdown pages and journals, reading their bytes as UTF-8 text, and replacing a
//! file's bytes through [`write.rs`](write). A content fingerprint is computed here because it is
//! a property of the file bytes.
//!
//! The write path is atomic in the filesystem sense: the target is replaced by renaming a synced
//! temporary file over it, so a reader sees old or new bytes, never a mixture, and a failed or
//! crashed write leaves the previous bytes in place. Locking and the durable move journal are
//! separate APIs under ADR-0004; raw text replacement does not take a lock or update the index.

mod config;
mod discovery;
mod error;
mod file;
mod intent;
mod journal;
mod lock;
mod write;

use std::io;
use std::path::{Path, PathBuf};

use crate::domain::JournalDate;

pub use config::journal_page_title_format;
use config::GraphConfig;
pub(crate) use config::NameFormat;

pub use discovery::PageFile;
pub(crate) use discovery::{kind_of_path, JOURNALS_DIR, PAGES_DIR};
pub use error::SourceError;
pub(crate) use file::content_fingerprint;
pub use file::FileMetadata;
pub use intent::MoveJournal;
pub(crate) use lock::{lock_directory, ScanLock};
pub use lock::{LockError, WriteLock};
pub(crate) use write::{after_commit, move_to_recycle};

/// An opened graph root and the entry point for reading its files.
///
/// Opening a graph resolves the root once and reads only the configuration that changes discovery.
/// Discovery and reads then use absolute paths, so a later working-directory change cannot break
/// them, and a caller never assembles graph paths itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSource {
    root: PathBuf,
    config: GraphConfig,
    signature: String,
}

impl GraphSource {
    /// Open the graph rooted at `root`.
    ///
    /// The root is canonicalized and must be an existing directory. A graph without
    /// `logseq/config.edn` gets Logseq's defaults; a configuration this build cannot honor is an
    /// error rather than a guess.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, SourceError> {
        let requested = root.into();
        let root = file::canonicalize(&requested)?;
        if !file::stat(&root)?.is_dir() {
            return Err(SourceError::Io {
                path: root,
                source: io::Error::from(io::ErrorKind::NotADirectory),
            });
        }

        let config = GraphConfig::load(&root)?;
        let signature = config.signature();
        Ok(Self {
            root,
            config,
            signature,
        })
    }

    /// Absolute, canonical path of the graph root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn signature(&self) -> &str {
        &self.signature
    }

    /// List every Markdown page and journal in the graph.
    ///    ///
    /// Pages come before journals, and each group is sorted by relative path, so repeated calls on
    /// an unchanged graph return the same order. Two files that share a comparison name are
    /// reported as [`SourceError::DuplicateName`].
    pub fn discover(&self) -> Result<Vec<PageFile>, SourceError> {
        discovery::discover(&self.root, &self.config)
    }

    /// Rebuild one [`PageFile`] from a graph-relative path, or `None` when no page lives there.
    ///
    /// This is the single-file form of [`GraphSource::discover`]: the path is resolved against the
    /// canonical root, stat'ed once, and classified and named by exactly the code discovery uses, so
    /// a file reached this way is the same [`PageFile`] discovery would have returned for it. No
    /// directory is read, which is the point: a caller that already knows which file it means — the
    /// index stores the path its projection came from — does not have to list the graph to find it.
    ///
    /// `None` is the answer for a location that no longer holds a page: the file is gone, it is not
    /// a regular file, it is not under `pages/` or `journals/`, or it is not a decodable Markdown
    /// page. A caller that must distinguish "the page moved" from "the page is gone" discovers the
    /// graph after this answers `None`. A file name the configuration cannot decode is still
    /// [`SourceError::InvalidPageName`], because that is a broken graph rather than a missing page.
    ///
    /// Only `pages/<file>` and `journals/<file>` are page locations, so any deeper or shallower path
    /// answers `None` before the filesystem is touched.
    pub fn page_at(&self, relative_path: &Path) -> Result<Option<PageFile>, SourceError> {
        discovery::page_at(&self.root, &self.config, relative_path)
    }

    /// Take a fresh stat of one file [`GraphSource::discover`] returned earlier, and nothing else.
    ///
    /// `file` must have come from this source's discovery: its `kind`, `name`, and `relative_path`
    /// are reused, and only [`PageFile::metadata`](PageFile::metadata) is refreshed. That is the
    /// operation a write needs after it replaces one file — the index must record the observation of
    /// the text it projects, not a snapshot taken before the replacement — and it costs one stat
    /// instead of a directory listing and a stat of every page in the graph.
    ///
    /// A file that disappeared between the discovery and this call is [`SourceError::Io`]: there is
    /// no page there to observe.
    pub fn reobserve(&self, file: &PageFile) -> Result<PageFile, SourceError> {
        discovery::reobserve(&self.root, file)
    }

    /// Replace the bytes of the file at `path` with `text`.
    ///
    /// `path` must be absolute and name a file whose parent directory exists, which is what
    /// [`PageFile::path`](PageFile::path) provides. The parent is resolved to its real location
    /// before the containment check, so a caller holding a path built from the same root it opened
    /// works whether or not that path went through a symlink; a relative path, a path with no file
    /// name, or a parent outside this graph root is refused rather than guessed at.
    ///
    /// The text is written exactly as given — no encoding or line-ending conversion — and is on
    /// disk (fsync) before the call returns.
    ///
    /// The replacement is atomic: the bytes land in a temporary file in the target's directory,
    /// are fsynced, and are renamed over the target, so a reader sees the old or the new bytes and
    /// a failure leaves the previous bytes in place. An existing target keeps its permissions; on
    /// failure any temporary file is removed. A target that is a symlink is replaced by a regular
    /// file, as every rename-based writer does; the symlink's destination is not written.
    ///
    /// This is a file replacement and nothing more: no lock is taken, no write intent is recorded,
    /// and no index is updated. Two concurrent writers to the same target can still race.
    pub fn write_text(&self, path: &Path, text: &str) -> Result<(), SourceError> {
        let target = self.resolve_target(path)?;
        write::write_text(&target, text)
    }

    /// How a page name is encoded in a file name.
    pub(crate) fn name_format(&self) -> NameFormat {
        self.config.name_format()
    }

    /// The page names `:favorites` lists, in written order.
    ///
    /// A name no page holds is still listed: the list is the graph's configuration, not a listing
    /// of pages, and the two facts are answered together by the read that needs both.
    pub(crate) fn favorites(&self) -> &[String] {
        self.config.favorites()
    }

    /// Graph-relative file path and displayed page title for the journal of `date`.
    ///
    /// Both halves come from the formats this graph was opened with, so the caller never spells a
    /// journal's name itself: `:journal/file-name-format` renders the second half of the
    /// `journals/<stem>.md` path and `:journal/page-title-format` renders the title, which is the
    /// name every read of that file answers with.
    ///
    /// This cannot fail. Both patterns were compiled and validated when [`GraphSource::open`] read
    /// the configuration — an unsupported one is an error there, not a silently degraded guess
    /// here — and rendering a compiled pattern for a day that exists has nothing to reject.
    pub(crate) fn journal_page(&self, date: JournalDate) -> (PathBuf, String) {
        let file_name = self.config.journal_file_name_format().format(date);
        let title = self.config.journal_page_title_format().format(date);
        (
            Path::new(JOURNALS_DIR).join(format!("{file_name}.md")),
            title,
        )
    }

    /// Create the file at `relative_path` with `text`, refusing to replace an existing file.
    ///
    /// `relative_path` is a plain graph-relative path such as `pages/Basic settings.md`; an absolute
    /// path, a parent directory component, or a root component is refused rather than resolved. The
    /// parent directory is created when it is missing, so the first page of an empty graph can be
    /// created. The resolved parent must remain inside the graph root, including when an existing
    /// ancestor is a symlink. The bytes are written exactly as given and are fsynced before return.
    ///
    /// This is a file creation and nothing more: no lock is taken and no index is updated. An
    /// existing file is [`SourceError::AlreadyExists`], never a replacement.
    pub fn create_text(&self, relative_path: &Path, text: &str) -> Result<(), SourceError> {
        if relative_path.is_absolute()
            || !relative_path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(SourceError::Io {
                path: relative_path.to_owned(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the path must be a plain graph-relative path",
                ),
            });
        }
        let path = self.root.join(relative_path);
        write::create_text(&self.resolve_target(&path)?, text)
    }

    pub(crate) fn remove_text(&self, path: &Path) -> Result<(), SourceError> {
        write::remove_text(&self.resolve_target(path)?)
    }

    /// Turn an absolute file path into a target inside the graph root.
    fn resolve_target(&self, path: &Path) -> Result<PathBuf, SourceError> {
        let invalid = |detail: &str| SourceError::Io {
            path: path.to_owned(),
            source: io::Error::new(io::ErrorKind::InvalidInput, detail.to_owned()),
        };

        if !path.is_absolute() {
            return Err(invalid(
                "the path must be absolute, as PageFile::path provides",
            ));
        }
        let Some(name) = path.file_name() else {
            return Err(invalid("the path names no file"));
        };
        let Some(parent) = path.parent() else {
            return Err(invalid("the path names no file"));
        };

        Ok(file::directory_within(&self.root, parent)?.join(name))
    }
}
