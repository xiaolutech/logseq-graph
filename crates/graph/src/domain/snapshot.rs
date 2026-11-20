//! One observation of a whole page: its structure, every block's editable text, its properties, and
//! the version of the bytes that produced them.
//!
//! The rest of the read surface answers one facet of a page per call — the tree from `index`, the
//! block text from `index`, the properties from `index` — and each answer carries the revision it
//! was read at. A caller that needs all of it therefore stitches several observations together, and
//! the pieces may come from different ones. This is the single-read shape instead: everything here
//! describes one page as one observation, so the parts cannot disagree with each other.
//!
//! Like the rest of the domain this is a value at one observation: no IO, no SQL, no storage handle.

use super::{BlockId, BlockMarker, BlockView, PageProperty};

/// Opaque version token of one page's source bytes.
///
/// A version identifies **one observation of the page's source bytes**: the token is the FNV-1a
/// 64-bit hash of the file's text, rendered as sixteen lowercase hexadecimal digits. It is not the
/// index revision, and the two move for different reasons: a revision advances whenever anything is
/// committed, while a version changes only when that page's own bytes change.
///
/// It is page-level: writing another page leaves it untouched, and so does any write that rewrites
/// this page with identical bytes.
///
/// Two tokens can only be compared for equality. A version is not parseable — the digits assert
/// nothing about the algorithm, and a later change to how it is computed is not a contract break as
/// long as equal bytes keep producing equal tokens. It is not ordered and not monotonic: a newer
/// version is not "greater", and `version != version` says the bytes differ, never which side is
/// newer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageVersion(String);

impl PageVersion {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// The token as text, for storage and comparison by a caller.
    ///
    /// Logging it and comparing it are the two things it is for. Do not read the digits as a number.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PageVersion {
    /// Reuse an opaque token returned by an earlier page read.
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for PageVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Two versions of one page: the bytes the index projected, and the bytes on disk now.
///
/// [`index`](Self::index) is the version of the text the page's rows were built from — the content a
/// [`Freshness::LastScan`](crate::engine::Freshness::LastScan) read answers with. [`disk`](Self::disk)
/// is the version of the file's current bytes, which is the one a
/// [`page_snapshot`](crate::engine::Graph::page_snapshot) of the same page reports. One token alone
/// cannot say whether the index is behind the file: two [`PageVersion`]s compare for equality and
/// nothing else, so which side is "newer" is not a fact this type carries.
///
/// What it does assert is the one thing equality already means. When the two are equal, the file's
/// bytes are the ones the index read, and a read of either side describes the same content. Every
/// other combination — a file edited since the scan, a page deleted from disk that the index still
/// lists — is a state the caller decides what to do about, because the decision differs: an editor
/// refreshes one page, a listing re-scans, a sync tool waits for the write that is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageVersions {
    index: PageVersion,
    disk: Option<PageVersion>,
}

impl PageVersions {
    pub(crate) fn new(index: PageVersion, disk: Option<PageVersion>) -> Self {
        Self { index, disk }
    }

    /// Version of the text the index's rows for this page were built from.
    ///
    /// It is the fingerprint the scan recorded for the file it projected, so it moves when this page
    /// is projected and stays put when another page is.
    pub fn index(&self) -> &PageVersion {
        &self.index
    }

    /// Version of the page file's current bytes, or `None` when no file holds this page any more.
    ///
    /// `None` is the answer to a page deleted from disk since the last scan, and it is not the same
    /// answer as "the page does not exist": [`index`](Self::index) still names what the index holds
    /// for it.
    pub fn disk(&self) -> Option<&PageVersion> {
        self.disk.as_ref()
    }
}

/// One page as a single observation, with its blocks nested in source order.
///
/// [`properties`](Self::properties) are the property lines of the page preamble, the text before
/// the first block, which is where Logseq writes page properties. A page that only states its
/// properties on its first block keeps them as that block's properties, exactly as
/// [`Graph::page_properties`](crate::engine::Graph::page_properties) reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSnapshot {
    page: String,
    version: PageVersion,
    properties: Vec<PageProperty>,
    blocks: Vec<SnapshotBlock>,
}

impl PageSnapshot {
    pub(crate) fn new(
        page: String,
        version: PageVersion,
        properties: Vec<PageProperty>,
        blocks: Vec<SnapshotBlock>,
    ) -> Self {
        Self {
            page,
            version,
            properties,
            blocks,
        }
    }

    /// Name of the page in the spelling the graph stores, which is the name its file decodes to.
    ///
    /// This is never the spelling the read was asked with, so it can be passed back to another read
    /// and name the same page.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Version of the source bytes this snapshot was parsed from.
    ///
    /// See [`PageVersion`] for what the token does and does not promise.
    pub fn version(&self) -> &PageVersion {
        &self.version
    }

    /// Property lines of the page preamble, in written order, duplicates included.
    ///
    /// A repeated key appears as often as it was written; deciding that the last line under a key
    /// wins stays the caller's rule.
    pub fn properties(&self) -> &[PageProperty] {
        &self.properties
    }

    /// The page's top-level blocks, in source order, each with its own subtree.
    pub fn blocks(&self) -> &[SnapshotBlock] {
        &self.blocks
    }
}

/// One block of a [`PageSnapshot`], with the position that names it and its own content.
///
/// A snapshot is a pure function of one file's bytes, so it carries no ephemeral id: that identity
/// lives in the index and is replaced whenever a page is reprojected (ADR-0012), while the same bytes
/// always produce the same snapshot. What the block carries is a position inside this observation —
/// [`child_path`](Self::child_path) — and the caller addresses it by a version token
/// ([`PageSnapshot::version`](super::PageSnapshot::version)) that says which bytes the position means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotBlock {
    child_path: Vec<u32>,
    preorder: u32,
    marker: BlockMarker,
    collapsed: bool,
    text: String,
    properties: Vec<PageProperty>,
    durable_id: Option<BlockId>,
    children: Vec<SnapshotBlock>,
}

impl SnapshotBlock {
    #[expect(
        clippy::too_many_arguments,
        reason = "the parameters are the eight fields of this struct in the order the one caller \
                  that builds a row holds them; a builder would name the same eight a second time"
    )]
    pub(crate) fn new(
        child_path: Vec<u32>,
        preorder: u32,
        marker: BlockMarker,
        collapsed: bool,
        text: String,
        properties: Vec<PageProperty>,
        durable_id: Option<BlockId>,
        children: Vec<SnapshotBlock>,
    ) -> Self {
        Self {
            child_path,
            preorder,
            marker,
            collapsed,
            text,
            properties,
            durable_id,
            children,
        }
    }

    /// Indices from the page root, zero-based, one element per level: the block's **child index**
    /// path.
    ///
    /// The first element is the block's position among the page's top-level blocks, the second its
    /// position among its parent's children, and so on; the length is the block's depth plus one.
    /// This is the positional address a batch of operations uses (ADR-0011): it means something only
    /// against the page version that produced it, which is why [`PageSnapshot`] states one.
    ///
    /// Deliberately not called `path`: a bare path in this crate is a preorder path
    /// ([`BlockLocator::Path`](crate::document::BlockLocator::Path)), and this is not that. The two
    /// cannot be compared or passed where the other is expected: a preorder path's first element is
    /// a preorder position, not a child index, so on the page `- a` / `\t- b` / `- c` the block `c`
    /// has `child_path == [1]` and a preorder path of `[2]`. Passing one where the other belongs
    /// names a different block, or none.
    ///
    /// This is also the address an ephemeral selectors' row is resolved to before an edit: the
    /// projection derives it from the block's parent chain, and the write applies it to the bytes
    /// the row was projected from after checking their fingerprint (ADR-0012).
    pub fn child_path(&self) -> &[u32] {
        &self.child_path
    }

    /// Depth-first position of the block within its page, starting at zero.
    ///
    /// The same position [`BlockView::preorder`](super::BlockView::preorder) and
    /// [`BlockNode::preorder`](super::BlockNode::preorder) report, so a snapshot's block and a tree
    /// read's node for the same block of the same page carry the same number. It is a position in
    /// one observation, not a stable block handle.
    pub fn preorder(&self) -> u32 {
        self.preorder
    }

    /// Marker that opens the block in the source, as the file wrote it.
    pub fn marker(&self) -> BlockMarker {
        self.marker
    }

    /// Whether the block states that it is folded.
    ///
    /// The block's `collapsed::` property says so, and the value that means folded is the one
    /// [`Graph::set_block_collapsed`](crate::engine::Graph::set_block_collapsed) writes. A block
    /// with no such property, with `false`, or with any other spelling, is unfolded.
    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// The block's content as an editor would show it: its own lines, without the marker,
    /// indentation, line endings, or property lines.
    ///
    /// This is [`BlockFacts::editable_text`](crate::document::BlockFacts::editable_text), so it is
    /// exactly the text an edit replaces. A block whose own lines are all property lines answers an
    /// empty string. The differences from the text the index stores are the property lines, which
    /// are reported separately by [`properties`](Self::properties), and nothing else: every other
    /// byte of the block's own lines is kept, blank lines and Markdown syntax included.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Property lines the block states, in written order, duplicates included.
    ///
    /// These are the lines [`text`](Self::text) leaves out, so a caller that re-renders the block
    /// from this snapshot has both halves of it.
    pub fn properties(&self) -> &[PageProperty] {
        &self.properties
    }

    /// The `id::` UUID the block states, if it states one that parses.
    ///
    /// A block with no `id::` line answers `None`, and so does one whose line holds anything but a
    /// `8-4-4-4-12` hexadecimal UUID: the line is ordinary text to every other reader, and a read
    /// never refuses a page over it.
    pub fn durable_id(&self) -> Option<&BlockId> {
        self.durable_id.as_ref()
    }

    /// The block's child blocks, in source order.
    pub fn children(&self) -> &[SnapshotBlock] {
        &self.children
    }
}

/// One block as the root of a read, together with the chain from the page root down to it.
///
/// This is what an editor that zooms into one block needs: the subtree it shows, and the breadcrumb
/// above it. [`root`](Self::root) is the selected block with its whole subtree attached, and
/// [`ancestors`](Self::ancestors) holds the blocks above it — **not** the selected block itself, so a
/// caller that renders the breadcrumb and then the root renders each block once.
///
/// The page and the version are the ones the whole snapshot was parsed from, so every block here
/// belongs to one observation of one file, exactly as in [`PageSnapshot`].
///
/// Like the rest of the domain this is a value at one observation: no IO, no SQL, no storage handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootedSnapshot {
    page: String,
    version: PageVersion,
    root: SnapshotBlock,
    ancestors: Vec<SnapshotBlock>,
}

impl RootedSnapshot {
    pub(crate) fn new(
        page: String,
        version: PageVersion,
        root: SnapshotBlock,
        ancestors: Vec<SnapshotBlock>,
    ) -> Self {
        Self {
            page,
            version,
            root,
            ancestors,
        }
    }

    /// Name of the page in the spelling the graph stores, which is the name its file decodes to.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Version of the source bytes this snapshot was parsed from.
    ///
    /// See [`PageVersion`] for what the token does and does not promise. It is the token the whole
    /// page's [`PageSnapshot`] carries at the same observation.
    pub fn version(&self) -> &PageVersion {
        &self.version
    }

    /// The selected block, with its entire subtree attached.
    ///
    /// Its positions are the page's, not the subtree's: `child_path` on this block and on every
    /// block under it still counts from the page root, exactly as it does in a [`PageSnapshot`] of
    /// the same page. A subtree read does not renumber anything, so a block keeps one path whichever
    /// read reported it.
    pub fn root(&self) -> &SnapshotBlock {
        &self.root
    }

    /// The blocks from the page's top level down to the selected block's parent, outer to inner.
    ///
    /// The selected block is not in this list: it is [`root`](Self::root). A top-level block
    /// therefore answers an empty chain.
    pub fn ancestors(&self) -> &[SnapshotBlock] {
        &self.ancestors
    }
}

/// What a `((block ref))` names (ADR-0005).
///
/// A reference is a durable id, and a durable id can name none, one, or too many blocks; this is
/// that answer in one value, so a renderer does not have to classify [`EngineError`] itself to tell
/// "this reference is dead" from "this reference is ambiguous". A caller that wants the block's own
/// content reads it from the [`BlockView`] a [`Found`](Self::Found) carries.
///
/// [`EngineError`]: crate::engine::EngineError
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockRefResolution {
    /// No block states the id: the block was deleted, or the id was never projected.
    Missing,
    /// More than one block states the id, so the reference names no single block.
    Ambiguous {
        /// Distinct page names stating the id, sorted and deduplicated.
        pages: Vec<String>,
    },
    /// Exactly one block states the id; this is it.
    Found {
        /// Name of the page the block belongs to, in the spelling the graph stores.
        page: String,
        /// The block the reference names.
        block: BlockView,
    },
}

impl BlockRefResolution {
    /// The page the reference resolved on, or `None` when it names no single block.
    pub fn page(&self) -> Option<&str> {
        match self {
            Self::Found { page, .. } => Some(page),
            Self::Missing | Self::Ambiguous { .. } => None,
        }
    }

    /// The block the reference names, or `None` when it names no single block.
    pub fn block(&self) -> Option<&BlockView> {
        match self {
            Self::Found { block, .. } => Some(block),
            Self::Missing | Self::Ambiguous { .. } => None,
        }
    }

    /// Names of the pages stating the id, sorted and deduplicated.
    ///
    /// Empty unless the answer is [`Ambiguous`](Self::Ambiguous): a miss and a hit involve no set of
    /// pages to report.
    pub fn ambiguous_pages(&self) -> &[String] {
        match self {
            Self::Ambiguous { pages } => pages,
            Self::Missing | Self::Found { .. } => &[],
        }
    }
}
