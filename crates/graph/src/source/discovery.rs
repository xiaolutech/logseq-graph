//! Discovery of Markdown page and journal files.
//!
//! Discovery is one directory listing per Logseq directory, with no recursion: Logseq stores pages
//! flat in `pages/` and journals flat in `journals/`. Root-level Markdown files, dot-prefixed
//! entries, and asset directories are not pages and are never returned.
//!
//! The two directories are named differently. A page is named by its file name, decoded with
//! `:file/name-format`. A journal is named by the date in its file name, read with
//! `:journal/file-name-format` and rendered with `:journal/page-title-format` (see
//! [`super::journal`]), so a journal's name is independent of the characters in its file name. A
//! journal file name that does not hold a date in the configured format is an error, not a page with
//! an odd name.
//!
//! Markdown is the only accepted source format. Entries with another extension are skipped before
//! their names are decoded, so an unrelated file with an undecodable name cannot fail discovery.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::{page_name_key, JournalDate, PageKind};

use super::config::GraphConfig;
use super::error::SourceError;
use super::file::{self, FileMetadata};

/// Directory under the graph root that holds regular pages.
pub(crate) const PAGES_DIR: &str = "pages";

/// Directory under the graph root that holds journal pages.
pub(crate) const JOURNALS_DIR: &str = "journals";

/// Extension accepted as Markdown source.
const MARKDOWN_EXTENSION: &str = "md";

/// One Markdown file that holds a page or a journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFile {
    kind: PageKind,
    name: String,
    resolved_name: Option<String>,
    date: Option<JournalDate>,
    comparison_name: String,
    relative_path: PathBuf,
    path: PathBuf,
    metadata: FileMetadata,
}

impl PageFile {
    /// Which kind of page this file holds.
    ///
    /// [`PageKind`] is a [`domain`](crate::domain) type: discovery decides the value from the
    /// directory, and every layer above reads it from here rather than re-deriving it.
    pub fn kind(&self) -> PageKind {
        self.kind
    }

    /// Name of the page, exactly as Logseq displays it.
    ///
    /// A page's name is its file name decoded by `:file/name-format`. A journal's name is its date
    /// rendered by `:journal/page-title-format`, such as `Aug 13th, 2026`; the file name that holds
    /// that date never appears in the name. Graph content is arbitrary Unicode, so the name is
    /// neither normalized nor case-folded here.
    pub fn name(&self) -> &str {
        self.resolved_name.as_deref().unwrap_or(&self.name)
    }

    pub(crate) fn fallback_name(&self) -> &str {
        &self.name
    }

    pub(crate) fn with_page_name(mut self, name: String) -> Self {
        self.comparison_name = page_name_key(&name);
        self.resolved_name = Some(name);
        self
    }

    pub(crate) fn name_in(&self, text: &str) -> String {
        crate::document::PageFacts::title_in(text)
            .unwrap_or_else(|| self.fallback_name().to_owned())
    }

    /// The calendar day this file is the journal of, or `None` for a regular page.
    ///
    /// The day is what the file name parses to under `:journal/file-name-format`, kept because
    /// rendering a title discards it and the title cannot be turned back into a day: two days can
    /// render the same title under some formats, and no reverse format is configured or guaranteed.
    /// It is therefore read from the file name at discovery and carried to the projection, never
    /// recovered from [`PageFile::name`].
    pub fn journal_date(&self) -> Option<JournalDate> {
        self.date
    }

    /// Key for comparing pages: [`page_name_key`] of the displayed name.
    ///
    /// The rule lives in `domain`, so this layer, the index's page key, and a `[[ref]]` target all
    /// fold a name the same way. Discovery rejects two files that share this key (see
    /// [`SourceError::DuplicateName`]). Because the key is derived from [`PageFile::name`], it is a
    /// page's decoded file name or a journal's formatted date title, and a page and a journal collide
    /// only when their displayed names collide.
    pub fn comparison_name(&self) -> &str {
        &self.comparison_name
    }

    /// Path relative to the graph root, such as `pages/Basic settings.md`.
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Absolute path of the file, resolved against the canonicalized graph root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Filesystem metadata recorded when the file was discovered.
    ///
    /// This is a snapshot taken before any read, and a caller that stores content together with
    /// metadata must record this snapshot rather than a later one. The file can change between
    /// discovery and the read, and an observation newer than the text it describes could cover a
    /// write the index never projected, so the next start would skip that write and serve the stale
    /// page. Recording the older snapshot costs at most one extra projection.
    pub fn metadata(&self) -> FileMetadata {
        self.metadata
    }

    /// Read this file's complete source text.
    ///
    /// The text is exactly the file's UTF-8 content: a byte order mark stays the first character,
    /// line endings stay as written, and a missing final newline stays missing. That is what
    /// [`PageDocument::parse`](crate::document::PageDocument::parse) and
    /// [`PageDocument::to_source`](crate::document::PageDocument::to_source) need to round-trip the
    /// file byte for byte. A file that cannot be read or is not UTF-8 is an error, never an empty
    /// page.
    pub fn read_text(&self) -> Result<String, SourceError> {
        file::read_text(&self.path)
    }
}

/// List every Markdown page and journal under the graph root.
///
/// The order is stable: pages first, then journals, each group sorted by relative path. A missing
/// directory contributes no files, because a graph may have no journals or no pages. Two files that
/// share a comparison name are an error rather than two entries.
pub(crate) fn discover(root: &Path, config: &GraphConfig) -> Result<Vec<PageFile>, SourceError> {
    let mut files = Vec::new();
    discover_directory(root, PAGES_DIR, PageKind::Page, config, &mut files)?;
    discover_directory(root, JOURNALS_DIR, PageKind::Journal, config, &mut files)?;
    reject_duplicate_names(&files)?;
    Ok(files)
}

/// Rebuild one [`PageFile`] from its graph-relative path, or `None` when no page lives there.
///
/// This is the single-file form of [`discover`]: it stats the one path and decodes its name, and it
/// reads no directory. A caller that already knows the path — the index stores the path a projection
/// came from — can therefore answer "what is this file now" without listing the graph again.
///
/// `None` covers every way the location stops holding a page: a missing file, a directory or other
/// non-file entry, a path outside `pages/` or `journals/` (the directory is the only classification
/// input), a dot-prefixed entry, and an entry that is not Markdown. A name the configuration cannot
/// decode is still an error, exactly as in [`discover`], because that is a broken graph rather than
/// a location without a page.
pub(crate) fn page_at(
    root: &Path,
    config: &GraphConfig,
    relative_path: &Path,
) -> Result<Option<PageFile>, SourceError> {
    let Some((directory, kind)) = page_location(relative_path) else {
        return Ok(None);
    };
    let Some(file_name) = relative_path.file_name() else {
        return Ok(None);
    };
    if file_name.as_encoded_bytes().first() == Some(&b'.') || !has_markdown_extension(file_name) {
        return Ok(None);
    }

    let path = root.join(relative_path);
    let stat = match fs::metadata(&path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(SourceError::Io { path, source }),
    };

    page_from_stat(&path, directory, kind, config, &stat)
}

/// The Logseq directory a graph-relative path is in, with the kind of page that directory holds.
///
/// The directory is the only classification input, and the graph's layout is this layer's knowledge,
/// so this rule lives here rather than in a caller that happens to hold a path. `None` means the path
/// is not a page location at all: a file only `pages/` or `journals/` holds is not a page, whatever
/// its name. [`page_at`] uses the directory to decode the name, and the projection stores the kind on
/// the page row, deriving it from the same path that names the file the row came from.
pub(crate) fn page_location(relative_path: &Path) -> Option<(&'static str, PageKind)> {
    match relative_path.parent().and_then(Path::to_str)? {
        PAGES_DIR => Some((PAGES_DIR, PageKind::Page)),
        JOURNALS_DIR => Some((JOURNALS_DIR, PageKind::Journal)),
        _ => None,
    }
}

/// Which kind of page a graph-relative path holds, or `None` when it is not a page location.
pub(crate) fn kind_of_path(relative_path: &Path) -> Option<PageKind> {
    page_location(relative_path).map(|(_, kind)| kind)
}

/// Take a fresh stat of one file an earlier [`discover`] returned.
///
/// A [`PageFile`] carries the stat observed before anything read it, and a caller that stores a
/// projection together with its observations has to record the observation of the text it actually
/// projected. Listing the whole graph for that one snapshot would stat every other file for nothing,
/// so a caller that already holds the `PageFile` re-observes exactly the one path it touched.
///
/// The name and the directory class are reused rather than derived again: they are properties of the
/// path, and the path has not changed. The path itself is rebuilt from `root` and the recorded
/// relative path, so a `PageFile` from another graph cannot make this stat a file outside this one.
///
/// A file that is gone, or that is no longer a regular file, is an error rather than a `PageFile`
/// with a missing stat: the caller asked for the observation of a page and there is no page there.
pub(crate) fn reobserve(root: &Path, file: &PageFile) -> Result<PageFile, SourceError> {
    let path = root.join(file.relative_path());
    let stat = file::stat(&path)?;
    if !stat.is_file() {
        return Err(SourceError::Io {
            path,
            source: io::Error::new(io::ErrorKind::InvalidInput, "no longer a regular file"),
        });
    }

    Ok(PageFile {
        path,
        metadata: file::record(&stat),
        ..file.clone()
    })
}

fn discover_directory(
    root: &Path,
    directory: &str,
    kind: PageKind,
    config: &GraphConfig,
    out: &mut Vec<PageFile>,
) -> Result<(), SourceError> {
    let dir = root.join(directory);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SourceError::Io { path: dir, source });
        }
    };

    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SourceError::Io {
            path: dir.clone(),
            source,
        })?;
        if let Some(page) = page_file(&entry, directory, kind, config)? {
            found.push(page);
        }
    }

    // `read_dir` order is unspecified, so sorting is what makes discovery reproducible.
    found.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    out.extend(found);
    Ok(())
}

/// Reject two files that a later layer could not tell apart.
///
/// `___` and `%2F` are two encodings of `/`, and names compare case-insensitively, so distinct file
/// names can decode to one `comparison_name`. The same holds across directories: a journal is named
/// by its formatted date title, so `journals/2026_08_13.md` and `pages/Aug 13th, 2026.md` are one
/// page to Logseq even though their file names differ. Reporting both files here keeps the collision
/// a property of the source layer instead of a uniqueness constraint discovered later in SQLite.
fn reject_duplicate_names(files: &[PageFile]) -> Result<(), SourceError> {
    let mut seen: HashMap<&str, &Path> = HashMap::with_capacity(files.len());
    for file in files {
        if let Some(first) =
            seen.insert(file.comparison_name.as_str(), file.relative_path.as_path())
        {
            return Err(SourceError::DuplicateName {
                name: file.comparison_name.clone(),
                first: first.to_path_buf(),
                second: file.relative_path.clone(),
            });
        }
    }
    Ok(())
}

fn page_file(
    entry: &fs::DirEntry,
    directory: &str,
    kind: PageKind,
    config: &GraphConfig,
) -> Result<Option<PageFile>, SourceError> {
    let path = entry.path();
    let file_name = entry.file_name();

    // Dot-prefixed entries are Logseq internals or editor leftovers, never pages. The first byte of
    // the encoded name is enough for an ASCII `.` on every platform.
    if file_name.as_encoded_bytes().first() == Some(&b'.') {
        return Ok(None);
    }
    // The extension filter runs before name decoding on purpose: a non-Markdown entry with a
    // non-UTF-8 name is not a page, and must not fail discovery for the rest of the graph.
    if !has_markdown_extension(&file_name) {
        return Ok(None);
    }

    // Symlinks are followed: a linked page is still a page. Directories named `*.md` are not.
    let stat = file::stat(&path)?;
    page_from_stat(&path, directory, kind, config, &stat)
}

/// Build the [`PageFile`] a path holds, or `None` when the path is not a page file after all.
///
/// This is the one place a file name becomes a page name, shared by discovery and `page_at`, so a
/// path found by listing the graph and a path resolved from the index cannot disagree about which
/// page a file holds. The caller has already filtered the entry by directory class, dot prefix, and
/// Markdown extension; what is left is the decode and the stat.
fn page_from_stat(
    path: &Path,
    directory: &str,
    kind: PageKind,
    config: &GraphConfig,
    stat: &fs::Metadata,
) -> Result<Option<PageFile>, SourceError> {
    if !stat.is_file() {
        return Ok(None);
    }
    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return Err(SourceError::InvalidPageName {
            path: path.to_owned(),
            reason: "Markdown file name is not valid UTF-8".to_owned(),
        });
    };
    let Some(stem) = markdown_stem(file_name) else {
        // A name like `.md` holds no stem and is not a page.
        return Ok(None);
    };

    let (date, name) = match kind {
        PageKind::Page => page_name(config, stem).map(|name| (None, name)),
        PageKind::Journal => journal_name(config, stem).map(|(date, name)| (Some(date), name)),
    }
    .map_err(|reason| SourceError::InvalidPageName {
        path: path.to_owned(),
        reason,
    })?;

    // The comparison key is the domain's page-name rule, not a second copy of it, so discovery and
    // the index cannot disagree about which names are one page.
    let comparison_name = page_name_key(&name);

    Ok(Some(PageFile {
        kind,
        comparison_name,
        date,
        name,
        resolved_name: None,
        relative_path: Path::new(directory).join(file_name),
        path: path.to_owned(),
        metadata: file::record(stat),
    }))
}

/// The name Logseq shows for a regular page: its file name decoded by `:file/name-format`.
fn page_name(config: &GraphConfig, stem: &str) -> Result<String, String> {
    let name = config
        .name_format()
        .decode(stem)
        .map_err(|error| error.to_string())?;
    // Logseq trims a file name's leading and trailing spaces; a name that is empty after trimming
    // cannot identify a page.
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("decoded page name is empty".to_owned());
    }
    Ok(name)
}

/// The day a journal file name states and the title Logseq shows for it.
///
/// Both answers come from one parse of the file name, so the day a `PageFile` carries and the name
/// it displays cannot disagree. The day is returned because it is the journal's identity and the
/// title is only its current rendering.
fn journal_name(config: &GraphConfig, stem: &str) -> Result<(JournalDate, String), String> {
    let date = config
        .journal_file_name_format()
        .parse_stem(stem)
        .map_err(|error| error.to_string())?;
    Ok((date, config.journal_page_title_format().format(date)))
}

/// Whether the entry's extension is Markdown, compared case-insensitively.
fn has_markdown_extension(file_name: &OsStr) -> bool {
    Path::new(file_name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(MARKDOWN_EXTENSION))
}

/// The file stem of a Markdown file name, or `None` for any other entry.
fn markdown_stem(file_name: &str) -> Option<&str> {
    let (stem, extension) = file_name.rsplit_once('.')?;
    if stem.is_empty() || !extension.eq_ignore_ascii_case(MARKDOWN_EXTENSION) {
        return None;
    }
    Some(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_extension_is_required_and_case_insensitive() {
        assert_eq!(markdown_stem("page.md"), Some("page"));
        assert_eq!(markdown_stem("page.MD"), Some("page"));
        assert_eq!(markdown_stem("dotted.name.md"), Some("dotted.name"));
        assert_eq!(markdown_stem("page.org"), None);
        assert_eq!(markdown_stem("page"), None);
        assert_eq!(markdown_stem(".md"), None);
    }

    #[test]
    fn markdown_extension_check_matches_the_stem_filter() {
        for name in [
            "page.md",
            "page.MD",
            "dotted.name.md",
            "page.org",
            "page",
            ".md",
        ] {
            assert_eq!(
                has_markdown_extension(OsStr::new(name)),
                markdown_stem(name).is_some(),
                "extension check disagrees with the stem filter for {name}"
            );
        }
    }
}
