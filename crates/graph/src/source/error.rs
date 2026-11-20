//! Failures raised while reading or writing graph files, or interpreting graph configuration.

use std::io;
use std::path::PathBuf;

/// Failure of a graph file operation.
///
/// Every variant names the file it concerns. Discovery and reads never fall back to an empty page
/// or a default name: a file that cannot be listed, read, or decoded fails the operation.
#[derive(Debug)]
pub enum SourceError {
    /// A file operation made the new file state visible, and a later durability step failed.
    ///
    /// `path` already holds the new text, or a create already made it exist, when this is reported;
    /// what is unconfirmed is that the change survives a crash. This is deliberately not folded into
    /// [`SourceError::Io`], because a caller that treats every source failure as "nothing was
    /// written" would retry an edit whose bytes are already on disk.
    Committed {
        /// File whose new state is visible.
        path: PathBuf,
        /// Path of the durability step that failed, such as the parent directory.
        step: PathBuf,
        /// Underlying filesystem error.
        source: io::Error,
    },
    /// The filesystem rejected an open, read, stat, or directory listing.
    Io {
        /// Path the operation addressed.
        path: PathBuf,
        /// Underlying filesystem error.
        source: io::Error,
    },
    /// A file's bytes are not valid UTF-8, so they cannot be parsed as Markdown text.
    NotUtf8 {
        /// Path of the undecodable file.
        path: PathBuf,
        /// Offset of the first invalid byte.
        valid_up_to: usize,
        /// Total length of the file in bytes.
        len: usize,
    },
    /// The configuration file is not readable as the supported EDN subset.
    InvalidConfig {
        /// Path of the configuration file.
        path: PathBuf,
        /// What the reader expected and where it stopped.
        detail: String,
    },
    /// The configuration selects a value this build deliberately does not implement.
    ///
    /// Guessing would silently disagree with Logseq, so the graph is refused instead.
    UnsupportedConfig {
        /// Path of the configuration file.
        path: PathBuf,
        /// Configuration key, with its leading colon.
        key: &'static str,
        /// Value found in the file.
        value: String,
    },
    /// A file name cannot be turned into the name Logseq shows for the page.
    ///
    /// A page file name that does not decode, and a journal file name that does not hold a valid
    /// date in the configured format, both land here.
    InvalidPageName {
        /// Path of the file.
        path: PathBuf,
        /// Why the name is unusable.
        reason: String,
    },
    /// The file a create asked for already exists, so nothing was written.
    ///
    /// Creating is `O_EXCL`: an existing file is never replaced, because a create has no previous
    /// bytes to keep and a caller that meant to write an existing page belongs on the replacement
    /// path instead.
    AlreadyExists {
        /// Path that already holds a file.
        path: PathBuf,
    },
    /// Two discovered files share a comparison name.
    ///
    /// The key is `PageFile::comparison_name`, the displayed page name lowercased. For a page that
    /// is its decoded file name; for a journal it is the date title `:journal/page-title-format`
    /// renders. Sharing it would make the two files indistinguishable to a later layer, so discovery
    /// reports both instead of returning a list that layer can only resolve by guessing.
    DuplicateName {
        /// Comparison name both files produce.
        name: String,
        /// First colliding file in discovery order.
        first: PathBuf,
        /// Later file with the same comparison name.
        second: PathBuf,
    },
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Committed {
                path, step, source, ..
            } => write!(
                formatter,
                "{} was written, but syncing {} failed, so the new state may not survive a crash: {source}",
                path.display(),
                step.display()
            ),
            Self::Io { path, source } => {
                write!(formatter, "cannot access {}: {source}", path.display())
            }
            Self::NotUtf8 {
                path,
                valid_up_to,
                len,
            } => write!(
                formatter,
                "{} is not valid UTF-8: invalid byte at offset {valid_up_to} of {len}",
                path.display()
            ),
            Self::InvalidConfig { path, detail } => write!(
                formatter,
                "{} is not readable as graph configuration: {detail}",
                path.display()
            ),
            Self::UnsupportedConfig { path, key, value } => write!(
                formatter,
                "{} sets {key} to {}, which this build does not support",
                path.display(),
                quoted(value)
            ),
            Self::InvalidPageName { path, reason } => write!(
                formatter,
                "{} has no usable page name: {reason}",
                path.display()
            ),
            Self::AlreadyExists { path } => write!(
                formatter,
                "{} already exists, and a create never replaces a file",
                path.display()
            ),
            Self::DuplicateName {
                name,
                first,
                second,
            } => write!(
                formatter,
                "{} and {} both name the page \"{name}\"",
                first.display(),
                second.display()
            ),
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Committed { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Render a configuration value so an empty value stays visible in a diagnostic.
fn quoted(value: &str) -> String {
    if value.is_empty() {
        "\"\"".to_owned()
    } else {
        format!("`{value}`")
    }
}
