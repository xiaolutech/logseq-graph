//! One block view: the content of a single block, with the selector that names it (ADR-0005).
//!
//! A view of one block carries its own text and structural marker, where it sits in its page, and
//! the [`BlockSelector`] that names it. The page's structure is [`PageTree`](super::PageTree)'s
//! answer, the block's children are not attached here, and the whole page text is never part of
//! this type.
//!
//! The selector is the point of the view. Every read that answers a [`BlockView`] answers one the
//! caller can hand straight back to a write — [`selector`](Self::selector) is the address of exactly
//! this block, so an edit never has to be preceded by a second read that re-derives one.
//!
//! Like the rest of the domain this is a value at one graph revision: no IO, no SQL, no storage
//! handle.

use super::{BlockId, BlockIdentityKind, BlockMarker, BlockSelector, EphemeralId};

/// One block of a page, with the selector that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockView {
    page: String,
    preorder: u32,
    depth: u32,
    parent_preorder: Option<u32>,
    text: String,
    marker: BlockMarker,
    selector: BlockSelector,
}

impl BlockView {
    pub(crate) fn new(
        page: String,
        preorder: u32,
        depth: u32,
        parent_preorder: Option<u32>,
        text: String,
        marker: BlockMarker,
        selector: BlockSelector,
    ) -> Self {
        Self {
            page,
            preorder,
            depth,
            parent_preorder,
            text,
            marker,
            selector,
        }
    }

    /// Name of the page the block belongs to, in the spelling the projection stored.
    ///
    /// This is the as-written name, never the spelling the read was asked with, so it can be passed
    /// back to another page or block read and name the same page.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Depth-first position of the block within its page, starting at zero.
    ///
    /// This is a position in the revision the read reports, not identity (ADR-0005): it orders one
    /// page's blocks and shifts when the structure above them changes. Use
    /// [`selector`](Self::selector) to name the block again after this read.
    pub fn preorder(&self) -> u32 {
        self.preorder
    }

    /// Nesting depth of the block, starting at zero for top-level blocks.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Preorder position of the parent block, or `None` for a top-level block.
    ///
    /// A caller walking from one block up to a page's top level needs this: the preorder alone says
    /// where the block sits, not which block contains it.
    pub fn parent_preorder(&self) -> Option<u32> {
        self.parent_preorder
    }

    /// The block's own content, as the projection reconstructed it.
    ///
    /// This is the block's text only, property lines included and nothing of its children or its
    /// page.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The structural marker that opens the block: a bullet or a heading of one through six levels.
    pub fn marker(&self) -> BlockMarker {
        self.marker
    }

    /// The selector that names this block, ready to be passed to a write.
    ///
    /// A block that states an `id::` answers [`BlockSelector::Durable`] with that id, and one that
    /// states none answers [`BlockSelector::Ephemeral`] with the id the projection minted for it.
    /// The selector is a value of this revision: the durable kind is still the block's address after
    /// the next write, and the ephemeral kind is not (ADR-0012).
    pub fn selector(&self) -> &BlockSelector {
        &self.selector
    }

    /// Which identity kind [`selector`](Self::selector) carries.
    ///
    /// ADR-0005 requires a public block view to say this, so a caller can tell a durable target from
    /// an ephemeral one without matching on the selector.
    pub fn identity(&self) -> BlockIdentityKind {
        self.selector.kind()
    }

    /// The `id::` UUID the block states, if it states one.
    ///
    /// This is the durable id of the block as the projection read it, which is exactly the durable
    /// selector a view of such a block carries. A block that states no `id::` answers `None` here
    /// and an ephemeral selector in its place.
    pub fn durable_id(&self) -> Option<&BlockId> {
        self.selector.durable_id()
    }

    /// The ephemeral id the projection minted for this block, if it has no `id::`.
    ///
    /// This is index-scoped: it exists while the index holds a row for this block and the page's
    /// bytes are the ones that row was projected from. It is never written to a file and never
    /// becomes durable identity.
    pub fn ephemeral_id(&self) -> Option<&EphemeralId> {
        self.selector.ephemeral_id()
    }
}
