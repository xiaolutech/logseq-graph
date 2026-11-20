use crate::domain::{BlockId, BlockProperties, BlockSelector, BlockSiblings, BlockView, Read};
use crate::engine::{EngineError, Freshness, Graph, Reads};
use crate::index::{DurableLookup, Index, IndexError};

impl Graph {
    /// One block addressed by a public target, or `None` when the target names no block.
    ///
    /// The two identity kinds answer differently on purpose (ADR-0005):
    ///
    /// - [`BlockSelector::Durable`] is the `id::` UUID a block states. A UUID no block states answers
    ///   `None`, not an error: the block may have been deleted, and "this id names nothing" is a
    ///   normal answer about durable identity.
    /// - [`BlockSelector::Ephemeral`] is the random id the projection minted for a block that states
    ///   none (ADR-0012). It names one block while the index holds its row and the page's bytes are
    ///   the ones that row was projected from, so an id the graph has moved past answers `None` — the
    ///   same absent answer as an unstated durable id — and never a neighbouring block. Nothing
    ///   re-derives or searches for a replacement.
    ///
    /// The view's [`BlockView::page`] is the page's stored spelling and
    /// [`BlockView::selector`] is the address the row carries: a durable selector for a block that
    /// states an `id::`, an ephemeral one for a block that does not. Instance and revision are not on
    /// the view: they are on the [`crate::domain::ReadMeta`] this result carries, so one read states them once.
    ///
    /// A durable id more than one block states fails with [`EngineError::AmbiguousBlockId`] instead
    /// of answering one of them. The projection records duplicates as they are, because one bad line
    /// must not hide a page; a read is where that becomes unanswerable, because it answers about the
    /// index and cannot open the file to tell a real duplicate from a stale row.
    ///
    /// Both kinds read under the same freshness policy as every other read, so a stale index refuses
    /// a block read exactly as it refuses a page read. A read never verifies a page's bytes, because
    /// it changes nothing: it answers what the index holds and says whether that is stale. A write is
    /// where the fingerprint of the bytes matters, because only a write can put an edit in the wrong
    /// block.
    pub fn block(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<Option<BlockView>>, EngineError> {
        self.block_with(self.freshness, selector)
    }

    /// Property lines of one block, in written order, or `None` when the target names no block.
    ///
    /// The selector is resolved by the same identity rule [`Graph::block`] applies, so both selector
    /// kinds answer and an ambiguous durable id fails with [`EngineError::AmbiguousBlockId`]. A target that names no
    /// block answers `None` — the same absent answer the block read gives — while a located block
    /// without property lines answers `Some` with an empty list.
    ///
    /// Every property line is one element, so a repeated key appears as often as it was written and
    /// "the last one wins" stays the caller's decision, exactly as for
    /// [`Graph::page_properties`]. The values are verbatim; what a `key:: value` line means is a
    /// later layer's decision, not this read's.
    ///
    /// Resolution and the property rows read under one freshness policy, so the block the selector
    /// located and the rows returned describe one revision.
    pub fn block_properties(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<Option<BlockProperties>>, EngineError> {
        self.block_properties_with(self.freshness, selector)
    }

    /// The blocks at the previous and the next sibling position of one block.
    ///
    /// Siblings are the blocks that share a parent, ordered by `sibling_index`; two top-level blocks
    /// are siblings of each other, so the rule is about the position, not the nesting. The previous
    /// answer is `None` at the first sibling position, the next one at the last: an edge of the
    /// page's structure, not a missing block. Each neighbour is a full [`BlockView`] with the same
    /// identity rule as [`Graph::page_blocks`], so a caller can chain reads from it.
    ///
    /// The selector resolves exactly as for [`Graph::block`]: both kinds answer and an ambiguous
    /// durable id fails with [`EngineError::AmbiguousBlockId`]. A target that names no block answers `None` for the whole
    /// pair. Both neighbours are located under one freshness policy, so the two views and the block
    /// they were found from describe one revision.
    pub fn block_siblings(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<BlockSiblings>, EngineError> {
        self.siblings_with(self.freshness, selector)
    }

    /// One block under `freshness`, reporting an ambiguous durable id as the error it is.
    ///
    /// An ambiguity is not an index failure, so it cannot travel out of the read closure as an
    /// [`IndexError`]. The closure answers with what the lookup found, the freshness policy and the
    /// metadata are settled exactly as for every other read, and only then does a target fact become
    /// [`EngineError::AmbiguousBlockId`].
    fn block_with(
        &mut self,
        freshness: Freshness,
        selector: &BlockSelector,
    ) -> Result<Read<Option<BlockView>>, EngineError> {
        let read = self.read_with(freshness, |index| locate_block(index, selector))?;
        let meta = read.meta().clone();
        Ok(Read::new(read.into_value().into_answer()?, meta))
    }

    /// Property lines of one block under `freshness`, reporting an ambiguous id as the error it is.
    ///
    /// The ambiguous-id error is shaped and settled exactly as in
    /// [`Self::block_with`]: the read closure answers with what it found, and only after the
    /// freshness policy has had its say does a fact about the target become an error.
    fn block_properties_with(
        &mut self,
        freshness: Freshness,
        selector: &BlockSelector,
    ) -> Result<Read<Option<BlockProperties>>, EngineError> {
        let read = self.read_with(freshness, |index| block_properties_with(index, selector))?;
        let meta = read.meta().clone();
        match read.into_value() {
            PropertyLookup::Found(rows) => Ok(Read::new(Some(rows), meta)),
            PropertyLookup::Missing => Ok(Read::new(None, meta)),
            PropertyLookup::Ambiguous { id, pages } => {
                Err(EngineError::AmbiguousBlockId { id, pages })
            }
        }
    }

    /// The sibling neighbours of one block under `freshness`, reporting an ambiguous id as the error
    /// it is.
    ///
    /// The ambiguous-id error is shaped and settled exactly as in
    /// [`Self::block_with`].
    fn siblings_with(
        &mut self,
        freshness: Freshness,
        selector: &BlockSelector,
    ) -> Result<Read<BlockSiblings>, EngineError> {
        let read = self.read_with(freshness, |index| {
            let view = match locate_block(index, selector)? {
                BlockLookup::Found { view, .. } => view,
                BlockLookup::Missing => return Ok(SiblingLookup::Missing),
                BlockLookup::Ambiguous { .. } => {
                    // A neighbour read has no per-case value to carry: the target fact becomes an
                    // engine error below, right where `block_with` turns it, so the closure answers
                    // with a marker that never reaches the caller.
                    return Ok(SiblingLookup::TargetError);
                }
            };
            match index.sibling_views(view.page(), view.preorder())? {
                Some(siblings) => Ok(SiblingLookup::Found(Box::new(siblings))),
                None => Err(IndexError::CorruptRow {
                    column: "pages.name_key",
                    value: view.page().to_owned(),
                }),
            }
        })?;
        let meta = read.meta().clone();
        match read.into_value() {
            SiblingLookup::Found(siblings) => Ok(Read::new(*siblings, meta)),
            SiblingLookup::Missing => Ok(Read::new((None, None), meta)),
            SiblingLookup::TargetError => {
                // Re-run the resolution outside the closure: the same selector, one later metadata
                // read, and the error carries the metadata the caller needs. A commit from another
                // process in between can only advance the revision, which is the direction every
                // read in this engine already allows.
                let read = self.read_with(freshness, |index| locate_block(index, selector))?;
                let meta = read.meta().clone();
                match read.into_value() {
                    BlockLookup::Ambiguous { id, pages } => {
                        Err(EngineError::AmbiguousBlockId { id, pages })
                    }
                    // The first resolution found the block, so a second pass cannot honestly answer
                    // anything else; the page or the block would have had to vanish mid-read, and a
                    // missing target is the answer that read gives rather than a guess.
                    BlockLookup::Found { .. } | BlockLookup::Missing => {
                        Ok(Read::new((None, None), meta))
                    }
                }
            }
        }
    }
}

impl Reads<'_> {
    /// One block addressed by a public target, or `None` when the target names no block.
    ///
    /// Same answers, same errors, and same freshness rule as [`Graph::block`].
    pub fn block(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<Option<BlockView>>, EngineError> {
        self.graph.block_with(self.freshness, selector)
    }

    /// Property lines of one block, in written order, or `None` when the target names no block.
    ///
    /// Same answers, same errors, and same freshness rule as [`Graph::block_properties`].
    pub fn block_properties(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<Option<BlockProperties>>, EngineError> {
        self.graph.block_properties_with(self.freshness, selector)
    }

    /// The blocks at the previous and the next sibling position of one block.
    ///
    /// Same answers, same errors, and same freshness rule as [`Graph::block_siblings`].
    pub fn block_siblings(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<BlockSiblings>, EngineError> {
        self.graph.siblings_with(self.freshness, selector)
    }
}

/// What one block read found, including the answers that must be reported as errors.
///
/// This is the answer of [`locate_block`], which runs inside the read closure and therefore returns
/// [`IndexError`] only. An ambiguous durable id is a fact about the target, not a failure of the
/// index, so it is carried here and turned into [`EngineError::AmbiguousBlockId`] by
/// [`Graph::block_with`] after the freshness policy has had its say.
pub(in crate::engine) enum BlockLookup {
    /// The target named one block.
    ///
    /// `child_path` is the position the ephemeral selector's row was projected at, and `None` for a
    /// durable one, whose identity needs no position. It travels with the view for the one read that
    /// parses a file — [`Graph::block_snapshot`] — which has to check a position from the index
    /// against a position in the bytes it just parsed.
    Found {
        /// The located block.
        view: BlockView,
        /// Child-index path of the row the ephemeral selector named.
        child_path: Option<Vec<u32>>,
    },
    /// The target named no block, which is a normal answer for both identity kinds.
    Missing,
    /// More than one block states the durable id the target names.
    ///
    /// The read cannot resolve this: it answers about the index and cannot open the file to tell a
    /// real duplicate from a stale row, so it reports every page involved and refuses.
    Ambiguous { id: BlockId, pages: Vec<String> },
}

impl BlockLookup {
    /// Separate the answer from the one target fact, which is an error.
    ///
    /// [`locate_block`] runs inside the read closure and can only return [`IndexError`], so an
    /// ambiguous durable id travels out as a variant of this enum. This is the one place that turns
    /// it into the error a caller sees, so the reads that address a block by a [`BlockSelector`]
    /// cannot word the same refusal two ways.
    pub(in crate::engine) fn into_answer(self) -> Result<Option<BlockView>, EngineError> {
        Ok(self.into_found()?.map(|found| found.view))
    }

    /// The same, keeping the position the row was projected at, for the read that parses a file.
    pub(in crate::engine) fn into_found(self) -> Result<Option<FoundBlock>, EngineError> {
        match self {
            Self::Found { view, child_path } => Ok(Some(FoundBlock { view, child_path })),
            Self::Missing => Ok(None),
            Self::Ambiguous { id, pages } => Err(EngineError::AmbiguousBlockId { id, pages }),
        }
    }
}

/// One located block, with the position its row was projected at.
///
/// The position travels beside the view because it is not part of a public block view: only
/// [`Graph::block_snapshot`] needs it, to check an index position against the file it parses.
pub(in crate::engine) struct FoundBlock {
    /// The located block.
    pub(in crate::engine) view: BlockView,
    /// Child-index path of the row an ephemeral selector named.
    pub(in crate::engine) child_path: Option<Vec<u32>>,
}

/// What one sibling-neighbours read found, including the answers that must be reported as errors.
///
/// Like [`BlockLookup`], the resolution runs inside the read closure and can only return
/// [`IndexError`], so the target fact is carried out as a marker and turned into an engine error by
/// [`Graph::siblings_with`] after the freshness policy has had its say.
enum SiblingLookup {
    /// The target named one block and its neighbours were read.
    Found(Box<BlockSiblings>),
    /// The target named no block.
    Missing,
    /// The target was ambiguous; re-resolution reports it as the engine error.
    TargetError,
}

/// Locate the block `selector` names, or say that it names none.
///
/// This only orchestrates, and its whole job is one index read per identity kind (ADR-0003): a
/// durable id through the durable-id lookup, an ephemeral id through the one index that holds it.
/// The two kinds do not share a resolution step because they share nothing: a durable id is answered
/// by the file's own statement of it, and an ephemeral id exists only while a row holds it. An
/// ephemeral id no row holds is a miss, the same absent answer a durable id no block states gets;
/// nothing looks for a block that resembles it (ADR-0009, ADR-0012).
pub(in crate::engine) fn locate_block(
    index: &Index,
    selector: &BlockSelector,
) -> Result<BlockLookup, IndexError> {
    match selector {
        BlockSelector::Durable(id) => Ok(match index.block_by_durable_id(id)? {
            DurableLookup::One(located) => BlockLookup::Found {
                view: BlockView::new(
                    located.page,
                    located.preorder,
                    located.depth,
                    located.parent_preorder,
                    located.text,
                    located.marker,
                    BlockSelector::Durable(located.durable_id),
                ),
                child_path: None,
            },
            DurableLookup::Absent => BlockLookup::Missing,
            DurableLookup::Several { pages } => BlockLookup::Ambiguous {
                id: id.clone(),
                pages,
            },
        }),
        BlockSelector::Ephemeral(id) => Ok(match index.block_by_ephemeral_id(id)? {
            Some(located) => BlockLookup::Found {
                view: BlockView::new(
                    located.page,
                    located.preorder,
                    located.depth,
                    located.parent_preorder,
                    located.text,
                    located.marker,
                    selector.clone(),
                ),
                child_path: Some(located.child_path),
            },
            None => BlockLookup::Missing,
        }),
    }
}

/// The property lines of one block, in written order.
///
/// The selector is resolved by the same [`locate_block`] the block read uses, so both identity kinds
/// answer and an ambiguous durable id refuses — the identity decision of ADR-0005 is made once, not
/// once per read.
///
/// Resolution and the property rows run inside one read, under one freshness policy, so the view the
/// properties describe and the properties themselves cannot come from different revisions. A page
/// that does not exist — or a durable id no block states — answers `None`, the same absent answer the
/// block read gives; a located block without property lines answers `Some` with an empty list.
fn block_properties_with(
    index: &Index,
    selector: &BlockSelector,
) -> Result<PropertyLookup, IndexError> {
    let view = match locate_block(index, selector)? {
        BlockLookup::Found { view, .. } => view,
        BlockLookup::Missing => return Ok(PropertyLookup::Missing),
        BlockLookup::Ambiguous { id, pages } => return Ok(PropertyLookup::Ambiguous { id, pages }),
    };
    let rows = index
        .block_properties(view.page(), view.preorder())?
        .ok_or(IndexError::CorruptRow {
            column: "pages.name_key",
            value: view.page().to_owned(),
        })?;
    Ok(PropertyLookup::Found(rows))
}

/// What one block-properties read found, including the answers that must be reported as errors.
///
/// This mirrors [`BlockLookup`]: the resolution runs inside the read closure and can only return
/// [`IndexError`], so an ambiguous durable id is carried out as a fact about the target and turned
/// into an engine error by [`Graph::block_properties_with`] after the freshness policy has had its
/// say.
enum PropertyLookup {
    /// The target named one block; these are its property lines in written order.
    Found(Vec<(String, String)>),
    /// The target named no block, which is a normal answer for both identity kinds.
    Missing,
    /// More than one block states the durable id the target names.
    Ambiguous { id: BlockId, pages: Vec<String> },
}
