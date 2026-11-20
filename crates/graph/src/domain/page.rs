//! Page and block views.

use super::block::BlockMarker;
use super::journal_date::JournalDate;
use super::kind::PageKind;

/// Derived metadata for one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMetadata {
    name: String,
    kind: PageKind,
    journal_date: Option<JournalDate>,
    block_count: u32,
    revision: u64,
    has_file: bool,
}

impl PageMetadata {
    pub(crate) fn new(
        name: String,
        kind: PageKind,
        journal_date: Option<JournalDate>,
        block_count: u32,
        revision: u64,
        has_file: bool,
    ) -> Self {
        Self {
            name,
            kind,
            journal_date,
            block_count,
            revision,
            has_file,
        }
    }

    /// Page name as recorded by the projection.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this page is a regular page or a journal.
    ///
    /// A journal's name is its formatted date title, not its identity, so this says which half of
    /// the graph the page came from and [`Self::journal_date`] says which day it is.
    pub fn kind(&self) -> PageKind {
        self.kind
    }

    /// The calendar day this page is the journal of, or `None` for a regular page.
    ///
    /// The day comes from the journal's file name, parsed with the configured
    /// `:journal/file-name-format` when the graph was discovered; it is never recovered from the
    /// displayed title, which is an arbitrary rendering that two days could share. `None` and
    /// [`Self::kind`] agree: a journal always has a day, and a regular page never does — the
    /// schema's CHECK enforces exactly that pairing.
    pub fn journal_date(&self) -> Option<JournalDate> {
        self.journal_date
    }

    /// Number of blocks in the page, at every depth.
    pub fn block_count(&self) -> u32 {
        self.block_count
    }

    /// Revision of the write that produced these rows.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether this page has a Markdown file rather than only being referenced.
    pub fn has_file(&self) -> bool {
        self.has_file
    }
}

/// One block and its children, in source order.
///
/// A block node carries structure only. Its text is stored in the derived read model and reachable
/// through a bounded search, but it is deliberately not attached here: a page-tree read returns
/// structure for the whole page, and loading every block's text with it would make one read cost as
/// much as the page's full content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockNode {
    preorder: u32,
    depth: u32,
    sibling_index: u32,
    marker: BlockMarker,
    children: Vec<BlockNode>,
}

impl BlockNode {
    pub(crate) fn new(
        preorder: u32,
        depth: u32,
        sibling_index: u32,
        marker: BlockMarker,
        children: Vec<BlockNode>,
    ) -> Self {
        Self {
            preorder,
            depth,
            sibling_index,
            marker,
            children,
        }
    }

    /// Depth-first position among the page's blocks, starting at zero.
    pub fn preorder(&self) -> u32 {
        self.preorder
    }

    /// Nesting depth, starting at zero for top-level blocks.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Position among the blocks that share this block's parent.
    pub fn sibling_index(&self) -> u32 {
        self.sibling_index
    }

    /// Marker that opened the block.
    pub fn marker(&self) -> BlockMarker {
        self.marker
    }

    /// Direct children in source order.
    pub fn children(&self) -> &[BlockNode] {
        &self.children
    }
}

/// One page as a page listing names it: the stored spelling, its kind, and its block count.
///
/// A summary is what a list of pages needs, not a page: it carries no block tree, no properties, and
/// no revision. Both kinds are listed by one read and in one order, so a caller that wants only
/// pages or only journals filters on [`PageSummary::kind`] instead of issuing a second read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSummary {
    name: String,
    kind: PageKind,
    journal_date: Option<JournalDate>,
    block_count: u32,
    has_file: bool,
}

impl PageSummary {
    pub(crate) fn new(
        name: String,
        kind: PageKind,
        journal_date: Option<JournalDate>,
        block_count: u32,
        has_file: bool,
    ) -> Self {
        Self {
            name,
            kind,
            journal_date,
            block_count,
            has_file,
        }
    }

    /// Page name as recorded by the projection.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this page is a regular page or a journal.
    pub fn kind(&self) -> PageKind {
        self.kind
    }

    /// The calendar day this page is the journal of, or `None` for a regular page.
    ///
    /// A listing carries it for the same reason it carries the kind: a journal's `name` is a
    /// rendering of this day, so ordering journals by time needs the day itself. The stored
    /// `YYYY-MM-DD` spelling sorts in time order, while the name does not — `Apr 19th, 2021` sorts
    /// before `Apr 1st, 2021` — so a caller sorting by name is sorting a label, not a date.
    pub fn journal_date(&self) -> Option<JournalDate> {
        self.journal_date
    }

    /// Number of blocks in the page, at every depth.
    pub fn block_count(&self) -> u32 {
        self.block_count
    }

    /// Whether this page has a Markdown file rather than only being referenced.
    pub fn has_file(&self) -> bool {
        self.has_file
    }
}

/// A page and its complete block tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTree {
    page: PageMetadata,
    blocks: Vec<BlockNode>,
}

impl PageTree {
    pub(crate) fn new(page: PageMetadata, blocks: Vec<BlockNode>) -> Self {
        Self { page, blocks }
    }

    /// Metadata of the page the tree belongs to.
    pub fn page(&self) -> &PageMetadata {
        &self.page
    }

    /// Top-level blocks in source order.
    pub fn blocks(&self) -> &[BlockNode] {
        &self.blocks
    }
}
