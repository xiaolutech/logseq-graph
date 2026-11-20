//! Block addresses and tree lookups.

use super::super::facts::{BlockFacts, PageFacts, ID_PROPERTY};
use super::super::line::LineSpan;
use super::super::parser::{Block, PageDocument};
use super::error::EditError;
use crate::domain::BlockMarker;

/// How one edit finds the block it applies to, inside one parsed page.
///
/// This type does not name [`BlockSelector`](crate::domain::BlockSelector), although the engine's
/// writes carry exactly one of its two identity kinds: a document cannot depend on the domain, and
/// it cannot express a handle's instance and revision either, because a page edit sees one page,
/// not a graph. What a page edit can answer is where a block is, so the three variants are the three
/// ways a caller can point at a block of one parsed page — by the `id::` value the block states, by
/// the preorder path ADR-0005 defines for ephemeral handles, or by a child-index path.
///
/// # The two paths, which this crate keeps confusing
///
/// [`Path`](Self::Path) and [`Indices`](Self::Indices) are both "a list of numbers" and are never
/// interchangeable. `Path`'s first element is a **preorder** position: the number of blocks, in
/// depth-first order, that precede the top-level block. `Indices`' first element is a **child
/// index**: the block's position among the page's top-level blocks. The first elements differ as
/// soon as one top-level block has children, because those children hold preorder numbers the
/// sibling positions never use. On `- a` / `\t- b` / `- c` the block `c` is `Path(&[2])` and
/// `Indices(&[1])`; `Path(&[1])` names `b`. Every later element means the same thing in both
/// spellings — the block's position among its parent's children — so only the first element is a
/// trap, and it is the one that turns a correct-looking path into an edit on the wrong block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockLocator<'a> {
    /// The block that states `id:: <0>` (ASCII case-insensitive).
    Id(&'a str),
    /// The block the preorder path names: `path[0]` is a top-level block's preorder, every later
    /// element a sibling position, one level per element.
    Path(&'a [u32]),
    /// The block the child-index path names: `path[0]` is the position among the page's top-level
    /// blocks, every later element the block's position among its parent's children, zero-based,
    /// one level per element.
    ///
    /// This is the positional path a batch of operations addresses blocks by: it is what
    /// [`SnapshotBlock::child_path`](crate::domain::SnapshotBlock::child_path) reports, so a caller
    /// that read a snapshot can point at any of its blocks without carrying an `id::`. It is a
    /// coordinate in one observation, not an identity — see [`Path`](Self::Path) for the other
    /// spelling and why the two cannot be mixed.
    Indices(&'a [u32]),
}

/// Where a moved block should end up, named by a locator for one existing block.
///
/// A position would be wrong by the time the edit runs: a preorder is a coordinate inside one
/// revision, and every edit changes it (ADR-0005), while the `id::` a block states survives the
/// move. A path locator is still correct here, because the edit reads it out of the file it is
/// about to transform — the same reason an ephemeral update or remove edits by path.
///
/// Four variants, and no more. "Move to the end of the page" is [`MoveTarget::After`] the last
/// top-level block, and a move inside one page always has a block to name; a fifth variant would
/// only give a caller a second way to say one thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveTarget<'a> {
    /// Immediately before the block this locator names.
    Before(BlockLocator<'a>),
    /// Immediately after the block this locator names, as its next sibling.
    ///
    /// "After the block" means after its whole subtree, not after its own lines: a block's
    /// children sit between it and its next sibling, so inserting after its own lines would make
    /// the moved block a child of it.
    After(BlockLocator<'a>),
    /// As the first child of the block this locator names.
    FirstChildOf(BlockLocator<'a>),
    /// As the last child of the block this locator names, after every existing child.
    LastChildOf(BlockLocator<'a>),
}

impl<'a> MoveTarget<'a> {
    /// The locator of the block this target is stated in terms of.
    ///
    /// Returned by reference: [`BlockLocator`] is not `Copy`, and the edit reads it where it
    /// resolves it.
    pub(super) fn anchor(&self) -> &BlockLocator<'a> {
        match self {
            Self::Before(anchor)
            | Self::After(anchor)
            | Self::FirstChildOf(anchor)
            | Self::LastChildOf(anchor) => anchor,
        }
    }
}

impl PageDocument {
    /// Own lines of the one block `locator` names.
    ///
    /// An id is matched against the facts, which are flat and order-independent; both path spellings
    /// are walked on the structure tree, whose nested `children` are the only place the level
    /// information lives. This is the one place a locator becomes a range of lines, so every edit
    /// above accepts every spelling without spelling out a resolution of its own.
    pub(super) fn block_span(
        facts: &PageFacts,
        document: &PageDocument,
        locator: &BlockLocator<'_>,
    ) -> Result<LineSpan, EditError> {
        match locator {
            BlockLocator::Id(block_id) => Self::block_span_by_id(facts, block_id),
            BlockLocator::Path(path) => Self::block_span_by_path(document, path),
            BlockLocator::Indices(indices) => Self::block_span_by_indices(document, indices),
        }
    }

    fn block_span_by_id(facts: &PageFacts, block_id: &str) -> Result<LineSpan, EditError> {
        let mut found: Option<LineSpan> = None;
        for block in facts.blocks() {
            let states_id = block
                .property(ID_PROPERTY)
                .is_some_and(|property| property.value().eq_ignore_ascii_case(block_id));
            if !states_id {
                continue;
            }
            if found.is_some() {
                return Err(EditError::AmbiguousBlockId);
            }
            found = Some(block.own_span());
        }
        found.ok_or(EditError::BlockNotFound)
    }

    /// Own lines of the one block the preorder path names, or an error when it names none.
    ///
    /// The path follows ADR-0005 exactly as the index's `block_by_page_path` reads it back:
    /// `path[0]` is the preorder of a top-level block, so only roots are candidates, and every later
    /// element is the position of that level's block among its siblings. Walking the parser's own
    /// tree keeps this the one rule for one page's structure, and a miss — an empty path, a preorder
    /// past the last root, a level with no child at that position — is an error rather than a
    /// neighbouring block, because a positional address that lands anywhere else would edit a block
    /// the caller never named.
    fn block_span_by_path(document: &PageDocument, path: &[u32]) -> Result<LineSpan, EditError> {
        let Some((root_preorder, deeper)) = path.split_first() else {
            return Err(EditError::PathOutOfRange);
        };
        // A root's preorder counts every block of the subtrees before it, so the top-level list is
        // walked with a running preorder counter instead of indexed by position. Matching the
        // counter against the path names the same roots the index's `block_by_page_path` does, and
        // a nested block's own preorder can never answer as if it were a root.
        let mut preorder = 0u32;
        let mut found = None;
        for root in document.blocks() {
            if preorder == *root_preorder {
                found = Some(root);
                break;
            }
            preorder += subtree_block_count(root);
        }
        let mut found = found.ok_or(EditError::PathOutOfRange)?;
        for &sibling in deeper {
            found = found
                .children()
                .get(sibling as usize)
                .ok_or(EditError::PathOutOfRange)?;
        }
        Ok(found.own_span())
    }

    /// Own lines of the one block the child-index path names, or an error when it names none.
    ///
    /// The first element indexes the page's top-level blocks by position; every later element
    /// indexes the children of the block the previous element named. That is the same walk
    /// [`SnapshotBlock::child_path`](crate::domain::SnapshotBlock::child_path) describes from the
    /// other end, so a path read out of a snapshot resolves to the block that snapshot reported —
    /// against the bytes it was read from, and against nothing else.
    ///
    /// An empty path, a top-level position, or a sibling position the level does not have is
    /// [`EditError::PathOutOfRange`] rather than a neighbouring block: the path is a coordinate, and
    /// a coordinate that lands anywhere else would edit a block the caller never named.
    fn block_span_by_indices(
        document: &PageDocument,
        indices: &[u32],
    ) -> Result<LineSpan, EditError> {
        let Some((root_index, deeper)) = indices.split_first() else {
            return Err(EditError::PathOutOfRange);
        };
        let mut found = document
            .blocks()
            .get(*root_index as usize)
            .ok_or(EditError::PathOutOfRange)?;
        for &sibling in deeper {
            found = found
                .children()
                .get(sibling as usize)
                .ok_or(EditError::PathOutOfRange)?;
        }
        Ok(found.own_span())
    }

    /// The preorder path of the block the locator names, in this document's current shape.
    ///
    /// This is the answer the engine's reads carry back with every block, and the one address a
    /// write can state about the block it edited: an edit that adds, replaces, or removes a
    /// property line moves the lines after the block but not the block's own place in the
    /// structure, so the path read before the edit is the path the next projection stores. It
    /// resolves the locator exactly as every edit does, so the errors are the same ones.
    pub fn block_path(&self, locator: BlockLocator<'_>) -> Result<Vec<u32>, EditError> {
        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        Ok(block_path(self, own_span))
    }

    pub(crate) fn block_child_path(
        &self,
        locator: BlockLocator<'_>,
    ) -> Result<Vec<u32>, EditError> {
        if let BlockLocator::Indices(indices) = locator {
            Self::block_span_by_indices(self, indices)?;
            return Ok(indices.to_vec());
        }
        let mut path = self.block_path(locator)?;
        let mut preorder = 0;
        path[0] = self
            .blocks()
            .iter()
            .position(|root| {
                let matches = preorder == path[0];
                preorder += subtree_block_count(root);
                matches
            })
            .and_then(|index| u32::try_from(index).ok())
            .ok_or(EditError::PathOutOfRange)?;
        Ok(path)
    }
}

/// The preorder path [`BlockLocator::Path`] reads: `path[0]` is a top-level block's preorder, and
/// every later element a sibling position, one level per element.
///
/// `target` is the block to name. The root preorder is the running count of the subtree sizes before
/// its root, the same walk [`PageDocument::block_span_by_path`] reads paths with, so a path this
/// builds and a path a caller later resolves name the same block of the same text.
pub(super) fn block_path(document: &PageDocument, target_span: LineSpan) -> Vec<u32> {
    let mut preorder = 0u32;
    for root in document.blocks() {
        if subtree_span_contains(root, target_span) {
            let mut path = vec![preorder];
            let mut current = root;
            while current.own_span() != target_span {
                #[expect(
                    clippy::expect_used,
                    reason = "the walk entered a root whose subtree holds `target_span` and stops at \
                              the block it equals, and a block's children partition the rest of its \
                              subtree span, so exactly one child holds it at each level"
                )]
                let (position, child) = current
                    .children()
                    .iter()
                    .enumerate()
                    .find(|(_, child)| subtree_span_contains(child, target_span))
                    .expect("a nested target descends through one child at each level");
                path.push(position as u32);
                current = child;
            }
            return path;
        }
        preorder += subtree_block_count(root);
    }
    unreachable!("the target is a block of this document")
}

/// Whether `span` lies anywhere inside `block` and its descendants.
pub(super) fn subtree_span_contains(block: &Block, span: LineSpan) -> bool {
    block.subtree_span().start() <= span.start() && span.end() <= block.subtree_span().end()
}

/// Number of blocks in `block`'s subtree, `block` itself included.
fn subtree_block_count(block: &Block) -> u32 {
    1 + block
        .children()
        .iter()
        .map(subtree_block_count)
        .sum::<u32>()
}

/// The lines from the start of the block holding `own` to the end of its last descendant.
///
/// `own` is the own span of one block in `document`, which is what
/// [`PageDocument::block_span_by_id`] returns. The parser already maintains every block's
/// [`subtree_span`](crate::document::Block::subtree_span) while it builds the tree, so the answer is
/// looked up instead of re-derived from indentation: depth is a property of the structure, and two
/// files can indent the same tree with tabs, with spaces, or with marker widths that differ.
///
/// This is the one rule for "which lines belong to this block and everything under it", and it is
/// shared on purpose. [`PageDocument::remove_block_text`] deletes exactly this range, and
/// `Graph::move_block` will take the same range out of the page it moves it to; a second copy of the
/// rule is a second answer waiting to disagree with the first.
pub(super) fn subtree_span(document: &PageDocument, own: LineSpan) -> LineSpan {
    document
        .blocks_in_preorder()
        .into_iter()
        .find(|block| block.own_span() == own)
        .map_or(own, |block| block.subtree_span())
}

/// Marker syntax and indent width of every block, in preorder.
///
/// Comparing this list before and after an edit is how the structural rule is enforced without
/// repeating the parser's marker and fence rules here: a continuation line that became a block adds
/// an entry, and a line that swallowed a following block removes one.
pub(super) fn block_shapes(document: &PageDocument) -> Vec<(BlockMarker, usize)> {
    document
        .blocks_in_preorder()
        .iter()
        .map(|block| (block.marker(), block.indent()))
        .collect()
}

/// Facts of the one block whose own lines are `own_span`.
///
/// Every locator is resolved through the same facts, so a span [`PageDocument::block_span`] answered
/// always has an entry here.
pub(super) fn facts_of(facts: &PageFacts, own_span: LineSpan) -> &BlockFacts {
    #[expect(
        clippy::expect_used,
        reason = "`PageFacts::extract` emits one `BlockFacts` per block of the document it is given, \
                  and every caller passes a span one of its walks answered, so the entry is there"
    )]
    facts
        .blocks()
        .iter()
        .find(|block| block.own_span() == own_span)
        .expect("a block resolved through these facts has facts")
}

/// The one block whose own lines are `own`, or `None` when this page holds no such block.
pub(super) fn find_block(document: &PageDocument, own: LineSpan) -> Option<&Block> {
    document
        .blocks_in_preorder()
        .into_iter()
        .find(|block| block.own_span() == own)
}
