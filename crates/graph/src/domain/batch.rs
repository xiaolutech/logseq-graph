//! One commit: a page, the version its paths were computed from, and an ordered batch of semantic
//! operations (ADR-0004, ADR-0011).
//!
//! A commit is the unit an editor actually has: several changes the user made in one gesture, which
//! must land together or not at all. The engine applies the operations in the order they appear,
//! writes the file once, re-reads it once, projects the page once, and advances one revision — so a
//! caller never sees a page that holds half a commit.
//!
//! Every operation addresses its block by **child index path**: the sequence of zero-based child
//! positions from the page root, the same path
//! [`SnapshotBlock::child_path`](super::SnapshotBlock::child_path) reports. The path is resolved
//! against the candidate document **in the state it has when that operation runs**, so the effects
//! of earlier operations are visible to later ones: an `InsertBlock` makes the new block addressable
//! for a following `EditText`, and an operation that deletes a block makes a later path that named
//! it a miss.
//!
//! A path is a **coordinate, not an identity**. It is only meaningful against the
//! [`PageVersion`](super::PageVersion) the caller read it from, which is why a commit carries
//! [`base_version`](PageCommit::base_version): the engine refuses the whole commit when the page's
//! own bytes are no longer that version, instead of interpreting the paths against a page they were
//! never computed from. Nothing here resolves a block by its durable `id::`; a batch is a positional
//! edit, and a caller that holds an id re-reads the page first.

use super::{BlockMarker, PageSnapshot, PageVersion};

/// One semantic operation against one page, applied in the order it appears in a commit.
///
/// Every variant names its block by child index path (see the `batch` module documentation); an
/// empty path names no block and is refused by every operation that takes one.
///
/// The operations are the document layer's pure edits, spelled without their byte mechanics: each
/// one maps to exactly one `PageDocument` edit, and that edit is where the rules for its refusals
/// live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageOperation {
    /// Replace the editable text of the block — what an editor shows, its property lines excluded.
    EditText {
        /// Child index path of the block.
        block: Vec<u32>,
        /// New editable text. A `\n` starts a continuation line.
        text: String,
    },
    /// Split the block in two at `offset` bytes of its editable text.
    ///
    /// The offset must land on a character boundary; the first half keeps the block's identity,
    /// properties and children, and the second half becomes a new block after its subtree.
    SplitBlock {
        /// Child index path of the block.
        block: Vec<u32>,
        /// UTF-8 byte offset into the block's editable text.
        offset: usize,
    },
    /// Fold the block into the block above it.
    MergeBlock {
        /// Child index path of the block.
        block: Vec<u32>,
    },
    /// Delete the block and lift its children into its place.
    UnwrapBlock {
        /// Child index path of the block.
        block: Vec<u32>,
    },
    /// Move the block and its subtree one indentation level deeper, under its previous sibling.
    IndentBlock {
        /// Child index path of the block.
        block: Vec<u32>,
    },
    /// Move the block and its subtree one indentation level shallower.
    OutdentBlock {
        /// Child index path of the block.
        block: Vec<u32>,
    },
    /// Mint one new block with single-line content.
    ///
    /// The new block is a sibling of the anchor; `after: None` appends it at the top level, at the
    /// end of the page's blocks. Its path in the candidate document is not reported: a later
    /// operation of the same commit addresses it by the position that operation's own path says,
    /// which the caller computed from the shapes it is building.
    InsertBlock {
        /// Anchor to insert after, or `None` to append to the top level.
        after: Option<Vec<u32>>,
        /// Content of the new block: one line, no line break.
        text: String,
    },
    /// Cut the block and its subtree out of the page and paste it beside another block.
    MoveBlock {
        /// Child index path of the block to move.
        block: Vec<u32>,
        /// Where it should end up.
        target: BatchMoveTarget,
    },
    /// Write the block's `collapsed::` property, the same way
    /// [`Graph::set_block_collapsed`](crate::engine::Graph::set_block_collapsed) does.
    ///
    /// `false` writes `collapsed:: false` rather than removing the line, because that is the byte
    /// the named write produces and a commit must not carry a second spelling of one state.
    SetCollapsed {
        /// Child index path of the block.
        block: Vec<u32>,
        /// Whether the block should read back as folded.
        collapsed: bool,
    },
    /// Rewrite the marker bytes that open the block: bullet to heading, heading to bullet, or one
    /// heading level to another.
    SetMarker {
        /// Child index path of the block.
        block: Vec<u32>,
        /// Marker the block should be written with.
        marker: BlockMarker,
    },
}

impl PageOperation {
    /// The variant's name, for an error that has to say which operation of a commit failed.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::EditText { .. } => "EditText",
            Self::SplitBlock { .. } => "SplitBlock",
            Self::MergeBlock { .. } => "MergeBlock",
            Self::UnwrapBlock { .. } => "UnwrapBlock",
            Self::IndentBlock { .. } => "IndentBlock",
            Self::OutdentBlock { .. } => "OutdentBlock",
            Self::InsertBlock { .. } => "InsertBlock",
            Self::MoveBlock { .. } => "MoveBlock",
            Self::SetCollapsed { .. } => "SetCollapsed",
            Self::SetMarker { .. } => "SetMarker",
        }
    }
}

/// Where a [`PageOperation::MoveBlock`] should put the block it moves.
///
/// The four positions are the document layer's `MoveTarget` variants with a child index path
/// instead of a locator. "After the block" means after its whole subtree, and "the end of the page"
/// is `After` the last top-level block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchMoveTarget {
    /// Immediately before the block the anchor path names, as its previous sibling.
    Before {
        /// Child index path of the anchor block.
        anchor: Vec<u32>,
    },
    /// Immediately after the block the anchor path names, as its next sibling.
    After {
        /// Child index path of the anchor block.
        anchor: Vec<u32>,
    },
    /// As the first child of the block the anchor path names.
    FirstChildOf {
        /// Child index path of the anchor block.
        anchor: Vec<u32>,
    },
    /// As the last child of the block the anchor path names, after every existing child.
    LastChildOf {
        /// Child index path of the anchor block.
        anchor: Vec<u32>,
    },
}

/// One commit: a page, the version its paths were computed from, and the operations.
///
/// The base version is the whole safety property of a positional batch. Paths describe one shape of
/// one page, and a page edited by someone else — another process, another window — has another
/// shape; applying these paths to those bytes would edit blocks the caller never named. The engine
/// therefore compares the version of the bytes it just read with [`base_version`](Self::base_version)
/// under the write lock and refuses the commit when they differ, without writing a byte.
///
/// A commit with no operations is legal and still passes that check: it writes nothing, and answers
/// the page as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCommit {
    page: String,
    base_version: PageVersion,
    operations: Vec<PageOperation>,
}

impl PageCommit {
    /// A commit against the page `page`, whose paths were computed from `base_version`'s bytes.
    pub fn new(
        page: impl Into<String>,
        base_version: PageVersion,
        operations: Vec<PageOperation>,
    ) -> Self {
        Self {
            page: page.into(),
            base_version,
            operations,
        }
    }

    /// Name of the page, in any spelling the graph resolves.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Version of the bytes the operation paths were read from.
    ///
    /// A commit is refused when the file is not at this version, because paths carry no identity to
    /// fall back on.
    pub fn base_version(&self) -> &PageVersion {
        &self.base_version
    }

    /// The operations, in the order they are applied.
    pub fn operations(&self) -> &[PageOperation] {
        &self.operations
    }
}

/// What a committed [`PageCommit`] answers: the page as the file holds it afterwards, and how many
/// operations were applied.
///
/// [`snapshot`](Self::snapshot) comes from the bytes the engine re-read after the replacement, not
/// from the candidate it assembled, so it is the page the next read will answer — one file
/// replacement, one observation, no second opinion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCommitResult {
    snapshot: PageSnapshot,
    applied: usize,
}

impl PageCommitResult {
    pub(crate) fn new(snapshot: PageSnapshot, applied: usize) -> Self {
        Self { snapshot, applied }
    }

    /// The whole page after the commit, with its version and its blocks.
    pub fn snapshot(&self) -> &PageSnapshot {
        &self.snapshot
    }

    /// Number of operations applied, which is the length of the commit's operations.
    pub fn applied(&self) -> usize {
        self.applied
    }
}
