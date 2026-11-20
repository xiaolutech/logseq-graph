use std::path::PathBuf;
use std::time::Duration;

use logseq_graph::domain::{BlockSelector, JournalDate};
use logseq_graph::engine::Freshness;

/// What one command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// Print the help text and stop.
    Help,
    /// Open the graph and run one command.
    Run(Invocation),
}

/// The shared options of every command and the command itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invocation {
    /// Graph root, when the command line names one.
    pub graph: Option<PathBuf>,
    /// Index database, when the command line names one.
    pub index: Option<PathBuf>,
    /// Freshness policy every read of this invocation uses.
    pub freshness: Freshness,
    /// The bearer tokens named by `--api-token`, which `serve` accepts and every other command
    /// refuses. `LOGSEQ_API_TOKEN` is read in `main`, where the environment is the process's.
    pub api_tokens: Vec<String>,
    /// What to print.
    pub command: Command,
}

/// The one command this invocation runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Graph identity, counts, index size, observation time, staleness.
    Info,
    /// Every page with its block count, and optionally the day a journal is.
    Pages { date: bool },
    /// One page's metadata and properties.
    Page { name: String },
    /// One page's blocks, indented by depth.
    Tree { name: String },
    /// Pages that reference one page.
    Backlinks { name: String },
    /// Every tag with its page count.
    Tags,
    /// Pages that use one tag.
    Tag { name: String },
    /// Pages under one namespace, directly or through the whole subtree.
    Namespace { name: String, tree: bool },
    /// Blocks whose text contains a query.
    Search { query: String, limit: u32 },
    /// Pages whose own name or `alias::` value contains a name.
    Find { query: String, limit: u32 },
    /// One Logseq simple query, answered from the projection.
    Query {
        /// The query as written, which the library parses.
        expr: String,
        /// How many matches to print.
        limit: u32,
        /// The day relative operands count from, or `None` for the system clock's day.
        ///
        /// `None` here rather than a date computed now, because this module is a pure function of
        /// the arguments: the one place that reads the clock is the binary.
        today: Option<JournalDate>,
    },
    /// One block by the selector it is addressed with.
    Block { selector: BlockSelector },
    /// Serve the read surface over HTTP until the process is killed.
    Serve {
        bind: String,
        poll_interval: Duration,
        no_watch: bool,
    },
    /// Create an empty page under `pages/`.
    Create { name: String },
    /// Create an empty journal for one calendar day.
    Journal { date: JournalDate },
    /// Apply one ordered batch to one page at a known version.
    Commit {
        page: String,
        base_version: String,
        operations: Vec<crate::batch::InputOperation>,
    },
    /// Append one block to one existing page.
    Append { page: String, text: String },
    /// Replace the text of one block.
    Update {
        selector: BlockSelector,
        text: String,
    },
    /// Delete one block and every block under it.
    Remove { selector: BlockSelector },
    /// Delete an inclusive run of siblings and their subtrees.
    RemoveRange {
        first: BlockSelector,
        last: BlockSelector,
    },
    /// Explicitly persist the block's durable identity.
    EnsureBlockId { selector: BlockSelector },
    /// Move one block, and every block under it, next to another block.
    Move {
        selector: BlockSelector,
        direction: MoveDirection,
        anchor: BlockSelector,
    },
}

/// The four positions [`Command::Move`] can name, as the command line spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveDirection {
    /// Immediately before the anchor block.
    Before,
    /// Immediately after the anchor block's subtree, as its next sibling.
    After,
    /// As the first child of the anchor block.
    FirstChild,
    /// As the last child of the anchor block.
    LastChild,
}

impl MoveDirection {
    /// The four words this grammar accepts, written the way every message lists them.
    ///
    /// One vocabulary for the command line and the errors: the usage message and the
    /// rejected-value message both use this string, so a caller that reads one can spell the other.
    pub(crate) const WORDS: &'static str = "before, after, first-child, or last-child";

    /// The direction one word names, or `None` for a word this grammar does not have.
    pub(super) fn parse(word: &str) -> Option<Self> {
        match word {
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            "first-child" => Some(Self::FirstChild),
            "last-child" => Some(Self::LastChild),
            _ => None,
        }
    }
}

impl Command {
    /// Only a write answers `false`. A read is served entirely from the index, so the startup scan is
    /// what makes its answer current; a write resolves the one file it edits and never derives its
    /// output from the index, so scanning every file first would buy it nothing. `serve` is a read
    /// that lives for many requests, so it pays the full projection once, at startup, and no served
    /// read repeats it (ADR-0008).
    pub(crate) fn needs_startup_scan(&self) -> bool {
        match self {
            Self::Append { .. }
            | Self::Create { .. }
            | Self::Journal { .. }
            | Self::Commit { .. }
            | Self::Update { .. }
            | Self::Remove { .. }
            | Self::EnsureBlockId { .. }
            | Self::RemoveRange { .. }
            | Self::Move { .. } => false,
            Self::Info
            | Self::Pages { .. }
            | Self::Page { .. }
            | Self::Tree { .. }
            | Self::Backlinks { .. }
            | Self::Tags
            | Self::Tag { .. }
            | Self::Namespace { .. }
            | Self::Search { .. }
            | Self::Find { .. }
            | Self::Query { .. }
            | Self::Block { .. }
            | Self::Serve { .. } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::invocation;

    #[test]
    fn only_the_writes_skip_the_startup_scan() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";

        for words in [
            &["info"][..],
            &["pages"],
            &["page", "alpha"],
            &["tree", "alpha"],
            &["backlinks", "alpha"],
            &["tags"],
            &["tag", "project"],
            &["namespace", "github.com/zhaochunqi/home-repo"],
            &["search", "needle"],
            &["block", id],
        ] {
            assert!(
                invocation(words).command.needs_startup_scan(),
                "{words:?} is a read and needs the scan that makes its answer current"
            );
        }

        for words in [
            &["append", "alpha", "text"][..],
            &["update", id, "text"],
            &["remove", id],
        ] {
            assert!(
                !invocation(words).command.needs_startup_scan(),
                "{words:?} is a write and resolves the file it edits without a scan"
            );
        }
    }
}
