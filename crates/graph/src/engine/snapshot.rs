//! One page in one read: its structure, every block's editable text, its properties, and the
//! version of the bytes they came from.
//!
//! The other page reads answer one facet each from the derived rows, and a caller that wants the
//! whole page has to stitch them together — which is why the version token exists: it names one
//! observation of the page's bytes, so a caller can tell whether two readings describe the same
//! state. This read parses the file itself, so structure, text, and properties come from one
//! observation by construction instead of by agreement.
//!
//! Nothing here is cached. Every call reads the file again, and every read that answers with a page
//! parses it again, which is the price of being correct against the file the index points at; a cache
//! would be a second copy of the page to keep in step, and this layer has no way to know when another
//! process edits the file. The one read here that answers without parsing is
//! [`Graph::page_versions`], because its question is about bytes rather than about blocks.

use std::path::Path;

use crate::document::{Block, BlockFacts, PageDocument, PageFacts, Property};
use crate::domain::{
    page_name_key, BlockId, BlockMarker, BlockRefResolution, BlockSelector, PageProperty,
    PageSnapshot, PageVersion, PageVersions, RootedSnapshot, SnapshotBlock,
};
use crate::index::IndexError;
#[cfg(doc)]
use crate::source::PageFile;

use super::{locate_block, BlockLookup, EngineError, Graph, Read};

impl Graph {
    /// One page as a single observation, or `None` when no page lives there.
    ///
    /// The answer holds the page's nested blocks in source order — each with the positions that name
    /// it, its marker, whether it is folded, its editable text, and its properties — together with
    /// the page's own properties and a [`PageVersion`] of the bytes it was parsed from. Every part
    /// describes one parse of one file, so unlike three separate reads they cannot come from
    /// different states of the page.
    ///
    /// `None` has three causes, and the last two are both stale rows rather than pages without
    /// blocks:
    ///
    /// - The index holds no page row for `name` (matched by
    ///   [`page_name_key`], so any spelling of a name names one page).
    ///   A page created after the last scan is this case, not the next one: nothing has projected it
    ///   yet.
    /// - The index holds a row, but the file it names is gone. Answering `None` is more honest than
    ///   reconstructing a page from rows whose file has disappeared; the next scan removes the row.
    /// - The index holds a row and a file, but that file no longer decodes to `name`, because
    ///   `logseq/config.edn` changed after the row was projected. The write path refuses the same row
    ///   for the same reason (`Graph::locate_page`), and this read must not answer another page's
    ///   content under the name it was asked for.
    ///
    /// # Freshness, and what the version is not
    ///
    /// The read runs under the graph's freshness policy exactly as every other read does, and only
    /// to answer one question: which file this page lives in. The page itself comes from that file's
    /// current bytes, so under [`Freshness::LastScan`](super::Freshness::LastScan) a file edited
    /// since the scan is still reported as it is now, while the row that located it is the scan's.
    /// [`PageVersion`] therefore identifies this read's view of the bytes, not the revision in the
    /// result's metadata; a caller that stores a version and compares it later is asking whether the
    /// file changed, not whether the index advanced.
    ///
    /// The snapshot mirrors the file, so it can nest deeper than [`MAX_BLOCK_DEPTH`] reports: a file
    /// edited since the last scan may nest deeper than the projection would accept, and this read
    /// does not apply the projection's limit to it.
    ///
    /// # What it does not do
    ///
    /// Under [`Freshness::LastScan`](super::Freshness::LastScan) and
    /// [`Freshness::AllowStale`](super::Freshness::AllowStale) it writes nothing: no file, no
    /// row, no `id::` line, and no revision. [`Freshness::CheckNow`](super::Freshness::CheckNow) and
    /// [`Freshness::VerifyContent`](super::Freshness::VerifyContent) scan the graph first, exactly as
    /// every other read under those
    /// policies does, so they commit whatever the scan found. The read itself never creates the page
    /// it could not find, which is what makes `None` an answer rather than an error.
    ///
    /// # An invariant this read asserts
    ///
    /// The block facts and the block structure come from one parse of one text and are visited in
    /// one depth-first order, so they cover the same blocks; `snapshot_from_text` asserts it. A
    /// mismatch is not a property of the file — this read would have no way to honour it — but of
    /// this build, so it panics instead of answering. That is deliberately different from the
    /// projection's `IndexError::CorruptRow`, which reports *stored data* that disagrees with the
    /// file: a caller may be able to repair that, and nothing a caller can do repairs this.
    ///
    /// [`MAX_BLOCK_DEPTH`]: crate::index::MAX_BLOCK_DEPTH
    pub fn page_snapshot(&mut self, name: &str) -> Result<Read<Option<PageSnapshot>>, EngineError> {
        // The freshness policy settles here, and its only answer is the file this page lives in.
        let located = self.read_with(self.freshness, |index| index.page_file(name))?;
        let meta = located.meta().clone();
        let Some(relative_path) = located.into_value() else {
            return Ok(Read::new(None, meta));
        };
        let Some((name, text)) = self.located_page_text(name, &relative_path)? else {
            return Ok(Read::new(None, meta));
        };
        Ok(Read::new(Some(snapshot_from_text(&name, &text)), meta))
    }

    /// One page's versions: the bytes the index projected, and the bytes on disk now.
    ///
    /// This is the read an editor polls while one page is open, and it answers the one question a
    /// snapshot cannot answer cheaply: has the file behind this page changed? Two tokens rather than
    /// one, because one token cannot say which side moved. A caller holding only the file's version
    /// sees that it is `v2` on every poll and cannot tell whether the index already holds `v2` or is
    /// stuck behind it; with both, equality answers that in one comparison, and inequality is the
    /// state that needs a scan. Which side is "newer" is deliberately not asserted — see
    /// [`PageVersions`] — because a [`PageVersion`] is comparable for equality and nothing else.
    ///
    /// Both tokens come from the same definitions [`Graph::page_snapshot`] uses: the file is
    /// located through the same row, stat'ed through `Graph::located_page_file`, and read with
    /// [`PageFile::read_text`]; the disk token is `page_version` of that text, which is what
    /// `snapshot_from_text` fingerprints, so a token from either read compares with a token from
    /// the other. The index token is `version_of_fingerprint` of the content fingerprint the scan
    /// recorded when it projected this page, so it is the same token the snapshot of that content
    /// reported, rendered by the same function.
    ///
    /// # No freshness policy
    ///
    /// This read takes no [`Freshness`](super::Freshness) and consults none, not even the graph's
    /// default. A policy settles the *index* — which scan's rows a read answers from — and the index
    /// side of this answer is the one thing a policy would change. Every policy would still pay for
    /// its scan, and `last` would read like an option to skip the file read, which is exactly what
    /// would make the disk token stop being the current bytes. The answer still states its own
    /// `stale` flag, because that flag is about whether the engine can vouch for the rows, not about
    /// the bytes, and it is the only warning a caller gets that the index side may be older than the
    /// observation time suggests.
    ///
    /// # Cost
    ///
    /// One seek on `pages.name_key`, one read of the baseline row, one stat, one read of the page
    /// file, and one hash of its bytes. None of that grows with the graph: no directory is listed,
    /// no baseline of another file is compared, and nothing is scanned for changes. That is a
    /// different order of cost from [`Freshness::CheckNow`](super::Freshness::CheckNow), which stats
    /// every file in the graph to answer the same question about one page.
    ///
    /// # When there is no answer
    ///
    /// `None` has the causes [`Graph::page_snapshot`] states — no page row for the name, a row whose
    /// file no longer decodes to the name — and one case a snapshot does not have: a page created
    /// since the last scan has no row, so nothing locates its file and this read answers that it
    /// knows no such page even though the file exists. A row whose file is *gone* is not `None`: it
    /// has a version on the index side and none on the disk side, which is the answer to a page
    /// deleted since the scan. This read creates neither a page nor a row.
    ///
    /// # What the answer does not depend on
    ///
    /// The page's structure, and whether it parses into a shape anyone would want. The disk token is
    /// the fingerprint of the file's text, so a file caught mid-write still has a version: it is the
    /// version of those bytes, which is what the caller asked for. A file that is not UTF-8 has no
    /// text to fingerprint and fails with the source error every other read and the scan report.
    pub fn page_versions(&mut self, name: &str) -> Result<Read<Option<PageVersions>>, EngineError> {
        self.refresh_index()?;
        self.index.snapshot(|index| {
            // No `settle` call: the metadata is read, but no policy runs. See this method's doc comment
            // for why a scan here would cost the caller graph-sized work and change nothing.
            let meta = self.read_meta(index.metadata()?, self.is_stale());
            let Some(relative_path) = index.page_file(name)? else {
                return Ok(Read::new(None, meta));
            };
            // The page rows and the baseline they were projected from are one observation, and
            // `pages.relative_path` is a foreign key into `file_baseline` with no `ON DELETE`, so a page
            // row without its baseline is a corrupt index rather than a state to answer from.
            let Some(baseline) = index.file_baseline(&relative_path)? else {
                return Err(IndexError::CorruptRow {
                    column: "file_baseline.content_hash",
                    value: relative_path.display().to_string(),
                }
                .into());
            };
            let index = version_of_fingerprint(baseline.content_hash());
            // A missing file is an answer here, not a miss: the index side is a version a caller can
            // still compare against the one it holds, and the page being gone is what it finds out.
            let disk = self
                .located_page_text(name, &relative_path)?
                .map(|(_, text)| page_version(&text));
            Ok(Read::new(Some(PageVersions::new(index, disk)), meta))
        })
    }

    /// The file a page row names, as one stat describes it, or `None` when that row no longer answers
    /// for `name`.
    ///
    /// This is the step the two reads that answer from a page's bytes share, and it exists so they
    /// cannot disagree about it: [`Graph::page_snapshot`] and [`Graph::page_versions`] locate the file
    /// by the same routine and refuse the same two stale rows.
    ///
    /// - A row whose file is gone is a stale row, not a page with no blocks.
    /// - A row can outlive a `logseq/config.edn` change that renames the pages its paths denote, and
    ///   a stat-trusting policy does not re-project for that. The write path refuses such a row for
    ///   the same reason (`Graph::locate_page`): answering with this file would report one page's
    ///   bytes under another page's name.
    fn located_page_text(
        &self,
        name: &str,
        relative_path: &Path,
    ) -> Result<Option<(String, String)>, EngineError> {
        let Some(file) = self.source.page_at(relative_path)? else {
            return Ok(None);
        };
        let text = file.read_text()?;
        let actual = file.name_in(&text);
        if page_name_key(&actual) != page_name_key(name) {
            return Ok(None);
        }
        Ok(Some((actual, text)))
    }

    /// The subtree one block roots, with the chain of blocks above it.
    ///
    /// This is the read an editor that zooms into a block needs: one observation of the file the
    /// block lives in, cut down to the block's own subtree, plus the breadcrumb from the page's top
    /// level down to it. The page and the [`PageVersion`] come with it, so the caller can tell which
    /// bytes it is looking at exactly as with [`Graph::page_snapshot`].
    ///
    /// The block is located by whichever identity the selector carries (ADR-0005), and the two
    /// differ in what they are used for here:
    ///
    /// - [`BlockSelector::Durable`] is looked up by the `id::` the snapshot states, never by the
    ///   position the index recorded: the file may have been edited since the scan, and a durable id
    ///   is the identity that survives a move. An id no block on that page states answers `None`,
    ///   and an id two blocks on that page state fails with [`EngineError::AmbiguousBlockId`] —
    ///   picking one would hand back a block the caller could not tell from the one it asked for.
    /// - [`BlockSelector::Ephemeral`] is located by the position its row was projected at: the
    ///   child-index path and the preorder both have to match a block in this observation. An id the
    ///   graph has written past has no row, so it answers `None` — the same absent answer a durable
    ///   id no block states gets (ADR-0012). An id whose row still exists is trusted only as far as
    ///   the file agrees: when another process restructured the file since the scan, the block at
    ///   that path is no longer the block at that preorder, and this answers `None` instead of the
    ///   block that stands there now. That is the aliasing ADR-0009 forbids, and it is the one place
    ///   a read can still see it.
    ///
    /// `None` has more causes than a selector that names nothing: the page row may be gone, its file
    /// may be gone, or the file may no longer decode to the page name the row stores, all for the
    /// reasons [`Graph::page_snapshot`] states. A path the file no longer holds — a page edited since
    /// the last scan under [`Freshness::LastScan`](super::Freshness::LastScan) — is also `None`, not a
    /// neighbouring block.
    ///
    /// The whole page is parsed to answer this, because the ancestor chain cannot be known without
    /// walking down from the page root. That is the price of the answer, and it is the same one
    /// [`Graph::page_snapshot`] pays; a subtree assembled incrementally would be a different read.
    ///
    /// # What it does not do
    ///
    /// Like [`Graph::page_snapshot`] it writes nothing of its own: no file, no row, no `id::` line.
    /// Under [`Freshness::CheckNow`](super::Freshness::CheckNow) and
    /// [`Freshness::VerifyContent`](super::Freshness::VerifyContent) the scan the policy runs before
    /// every read may commit what it observed, as for every other read in this engine.
    pub fn block_snapshot(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<Option<RootedSnapshot>>, EngineError> {
        // One read answers both halves of the first step: which block the selector names, and which
        // file that block's page lives in. The page path is folded into the same closure on purpose —
        // a second `read_with` would pay for a second scan of the whole graph under
        // `Freshness::CheckNow` and `Freshness::VerifyContent`, and could report a block from one
        // observation beside a file from a later one.
        let read = self.read_with(self.freshness, |index| {
            let lookup = locate_block(index, selector)?;
            let page_file = match &lookup {
                BlockLookup::Found { view, .. } => index.page_file(view.page())?,
                BlockLookup::Missing | BlockLookup::Ambiguous { .. } => None,
            };
            Ok((lookup, page_file))
        })?;
        let meta = read.meta().clone();
        let (lookup, page_file) = read.into_value();
        // `into_found` is where an ambiguous id becomes the error `Graph::block` reports, so this
        // read cannot word the same refusal differently. The child path comes with the view: it is
        // the position the ephemeral selector's row was projected at, and an ephemeral selector is
        // the only kind that has one.
        let Some(found) = lookup.into_found()? else {
            return Ok(Read::new(None, meta));
        };
        let view = found.view;
        let projected_path = found.child_path;
        let page = view.page().to_owned();
        let Some(relative_path) = page_file else {
            return Ok(Read::new(None, meta));
        };
        // The same two refusals `Graph::page_snapshot` makes, through the same routine.
        let Some((name, text)) = self.located_page_text(&page, &relative_path)? else {
            return Ok(Read::new(None, meta));
        };
        let snapshot = snapshot_from_text(&name, &text);

        let found = match selector {
            // Identity, not position: the file may have changed the block's preorder since the scan
            // that projected the row, and an `id::` still names the same block wherever it moved.
            BlockSelector::Durable(id) => {
                let found = matching(snapshot.blocks(), |block| block.durable_id() == Some(id));
                // One file may state the same `id::` twice, and this read cannot tell a real
                // duplicate from a copy-paste that kept the identity line. One page involved means
                // both blocks live here, so it is named once.
                if found.len() > 1 {
                    return Err(EngineError::AmbiguousBlockId {
                        id: id.clone(),
                        pages: vec![snapshot.page().to_owned()],
                    });
                }
                found
            }
            // The row still holds the id, but that only says the index has not been reprojected:
            // another process may have restructured the file since the scan. Two position facts from
            // that row settle it — the child-index path and the preorder — and both have to hold,
            // because a file that gained or lost a block satisfies one of them by accident. A block
            // that matches one but not the other is the aliasing this read refuses to answer.
            BlockSelector::Ephemeral(_) => {
                let projected = view.preorder();
                let path = projected_path.as_deref().unwrap_or_default();
                matching(snapshot.blocks(), |block| {
                    block.child_path() == path && block.preorder() == projected
                })
            }
        };
        let Some((ancestors, root)) = found.into_iter().next() else {
            return Ok(Read::new(None, meta));
        };
        Ok(Read::new(
            Some(RootedSnapshot::new(
                snapshot.page().to_owned(),
                snapshot.version().clone(),
                root.clone(),
                ancestors.into_iter().cloned().collect(),
            )),
            meta,
        ))
    }

    /// What a `((block ref))` names: no block, more than one, or exactly this block.
    ///
    /// This is [`Graph::block`] addressed by a durable id, with the third answer `block` reports as an
    /// error turned into a value: a reference to an id that two blocks state is a fact about the
    /// graph, not a failed read, and a renderer asks about every reference it draws. The pages
    /// stating the id come with the ambiguous answer, sorted and deduplicated, which is what a person
    /// needs to go find the duplicate.
    ///
    /// A position cannot be the answer here: the selector is always durable, so nothing about this
    /// read depends on where a block sits in its page.
    pub fn resolve_block_ref(
        &mut self,
        id: &BlockId,
    ) -> Result<Read<BlockRefResolution>, EngineError> {
        let selector = BlockSelector::Durable(id.clone());
        let read = self.read_with(self.freshness, |index| locate_block(index, &selector))?;
        let meta = read.meta().clone();
        let resolution = match read.into_value() {
            // Unlike `Graph::block`, this read answers an ambiguity as a value, so the pages are taken
            // before the shared conversion would turn the lookup into that error.
            BlockLookup::Ambiguous { pages, .. } => BlockRefResolution::Ambiguous { pages },
            lookup => match lookup.into_answer()? {
                Some(view) => BlockRefResolution::Found {
                    page: view.page().to_owned(),
                    block: view,
                },
                None => BlockRefResolution::Missing,
            },
        };
        Ok(Read::new(resolution, meta))
    }
}

/// Every block of `blocks` that `matches` accepts, each with the chain of blocks above it.
///
/// The chain is outer to inner and stops at the matched block's parent, so it never holds the block
/// itself. One traversal answers both, because the chain only exists while walking down: collecting
/// it afterwards would mean walking the tree a second time by a second rule, and the two walks could
/// disagree.
///
/// The traversal is iterative because the depth is a property of the file, not of this build: a page
/// edited since the last scan can nest far deeper than the projection accepts, and the thread stack
/// must not be what decides whether it can be read.
fn matching<'a>(
    blocks: &'a [SnapshotBlock],
    matches: impl Fn(&SnapshotBlock) -> bool,
) -> Vec<(Vec<&'a SnapshotBlock>, &'a SnapshotBlock)> {
    let mut found = Vec::new();
    // The chain above the frame being walked; its length is always the stack's length minus one.
    let mut ancestors: Vec<&'a SnapshotBlock> = Vec::new();
    let mut stack: Vec<std::slice::Iter<'a, SnapshotBlock>> = vec![blocks.iter()];
    while let Some(frame) = stack.last_mut() {
        let Some(block) = frame.next() else {
            stack.pop();
            // The frame that just ended is the one whose blocks `ancestors` last names; at the page
            // root there is none, and popping the empty chain is what keeps the two in step.
            ancestors.pop();
            continue;
        };
        if matches(block) {
            found.push((ancestors.clone(), block));
        }
        if !block.children().is_empty() {
            ancestors.push(block);
            stack.push(block.children().iter());
        }
    }
    found
}

/// Version token of one page's text.
///
/// This is the only place a version is computed from a page's bytes, so the token a caller compares
/// later and the token a snapshot reports cannot drift apart. The hash is the source layer's content
/// fingerprint — the same FNV-1a 64-bit value a scan compares a file against — rendered by
/// [`version_of_fingerprint`].
pub(crate) fn page_version(text: &str) -> PageVersion {
    version_of_fingerprint(crate::source::content_fingerprint(text))
}

/// The one rendering of a content fingerprint as a [`PageVersion`].
///
/// Two things produce a version: text being read now, and the fingerprint a scan stored when it
/// projected that text. They are one definition because they must be comparable: a caller's
/// `version == disk_version` compares a token from the index with a token from the file, and two
/// renderings of the same number that differed by a leading zero would report a page as changed
/// forever. Fixed width, so the token's shape does not depend on the value.
pub(crate) fn version_of_fingerprint(fingerprint: u64) -> PageVersion {
    PageVersion::new(format!("{fingerprint:016x}"))
}

/// Parse `text` as the page `page` and describe all of it in one go.
///
/// The caller is responsible for `page` being the name the graph stores for these bytes; nothing
/// here can tell whether it is.
///
/// The assertion this function makes comes from its own construction: one `PageDocument::parse` of
/// one text feeds both the structure walk and [`PageFacts::extract`], and this function walks them
/// together in the same depth-first order. A disagreement between them therefore cannot be caused
/// by an input, only by a defect in this build, and the panic with its message is what says so.
pub(crate) fn snapshot_from_text(page: &str, text: &str) -> PageSnapshot {
    let document = PageDocument::parse(text);
    let facts = PageFacts::extract(&document);

    // One depth-first walk fills every position. `facts` and the parsed blocks cover the same
    // document in the same order, so the fact at the walk's index belongs to the block it is looking
    // at; a mismatch means two walks of one document disagree, which is a defect in this build
    // rather than a property of the input. Refusing it loudly keeps a snapshot from attaching one
    // block's text to another block's position.
    let mut flat: Vec<FlatBlock> = Vec::new();
    let mut stack = vec![OpenFrame::root(&document)];
    while let Some(frame) = stack.last_mut() {
        let Some(block) = frame.children.next() else {
            stack.pop();
            continue;
        };
        let sibling_index = frame.next_sibling;
        frame.next_sibling += 1;

        let index = flat.len();
        #[expect(
            clippy::expect_used,
            reason = "one `PageDocument::parse` of one text feeds both walks, and `PageFacts::extract` \
                      emits one entry per block of the document it is given, so the structure walk \
                      cannot outrun the facts list; the `assert_eq!` below is what names a \
                      disagreement instead of indexing with it"
        )]
        let fact = facts.blocks().get(index).expect(
            "the fact walk covers every block the structure walk finds: a walk that stops short is \
             a defect in this build, not in this file",
        );
        assert_eq!(
            fact.own_span(),
            block.own_span(),
            "the fact walk and the structure walk must visit the same blocks in the same order; \
             this build is broken, not this file"
        );

        // The child-index path starts at the block's position among the top-level blocks, and each
        // deeper element is the block's position among its siblings. It continues its parent's path,
        // which this walk has already recorded: a parent precedes its children in preorder.
        let child_path = match frame.parent {
            None => vec![sibling_index],
            Some(parent) => extend(&flat[parent].child_path, sibling_index),
        };
        flat.push(FlatBlock {
            child_path,
            preorder: index as u32,
            marker: block.marker(),
            collapsed: collapsed(fact),
            text: fact.editable_text(),
            properties: properties(fact.properties()),
            durable_id: fact.durable_id(),
            parent: frame.parent,
        });
        stack.push(OpenFrame::under(block, index));
    }
    assert_eq!(
        flat.len(),
        facts.blocks().len(),
        "the fact walk and the structure walk must cover the same blocks; this build is broken, \
         not this file"
    );

    // Blocks arrive in preorder, so a child always follows its parent. Filling the tree from the
    // back therefore assembles every subtree before the block that owns it, and the whole
    // construction stays iterative: document depth is bounded by memory, not by the thread stack.
    let mut nested: Vec<Vec<SnapshotBlock>> = flat.iter().map(|_| Vec::new()).collect();
    let mut roots: Vec<SnapshotBlock> = Vec::new();
    for (index, block) in flat.into_iter().enumerate().rev() {
        // Children were pushed back to front, because the loop walks preorder in reverse.
        let mut children = std::mem::take(&mut nested[index]);
        children.reverse();
        let node = SnapshotBlock::new(
            block.child_path,
            block.preorder,
            block.marker,
            block.collapsed,
            block.text,
            block.properties,
            block.durable_id,
            children,
        );
        match block.parent {
            Some(parent) => nested[parent].push(node),
            None => roots.push(node),
        }
    }
    roots.reverse();

    PageSnapshot::new(
        page.to_owned(),
        page_version(text),
        properties(facts.properties()),
        roots,
    )
}

/// One block of the walk, before its subtree is attached.
struct FlatBlock {
    child_path: Vec<u32>,
    preorder: u32,
    marker: BlockMarker,
    collapsed: bool,
    text: String,
    properties: Vec<PageProperty>,
    durable_id: Option<BlockId>,
    /// Index of the parent in the flat list; `None` for a top-level block.
    parent: Option<usize>,
}

/// One level of the depth-first walk: the siblings still to visit and where they hang.
///
/// A frame carries no path of its own: the parent's path is already in the flat list by the time its
/// children are read, so a child continues it by index instead of by a copy.
struct OpenFrame<'a> {
    children: std::slice::Iter<'a, Block>,
    /// Flat index of the block this frame belongs to; `None` for the page root.
    parent: Option<usize>,
    /// Position the next child takes among its siblings.
    next_sibling: u32,
}

impl<'a> OpenFrame<'a> {
    /// The frame for the page's top level, which hangs under no block.
    fn root(document: &'a PageDocument) -> Self {
        Self {
            children: document.blocks().iter(),
            parent: None,
            next_sibling: 0,
        }
    }

    /// The frame for the children of `block`, which the walk read at flat index `parent`.
    fn under(block: &'a Block, parent: usize) -> Self {
        Self {
            children: block.children().iter(),
            parent: Some(parent),
            next_sibling: 0,
        }
    }
}

/// `prefix` with `last` appended, as a fresh path.
fn extend(prefix: &[u32], last: u32) -> Vec<u32> {
    let mut path = Vec::with_capacity(prefix.len() + 1);
    path.extend_from_slice(prefix);
    path.push(last);
    path
}

/// Map the parsed property lines of a page or a block into the domain's spelling of the same thing.
///
/// Both layers keep one element per written line, in source order, with the key as written and the
/// normalized key beside it, so this is a copy rather than a rule.
fn properties(parsed: &[Property]) -> Vec<PageProperty> {
    parsed
        .iter()
        .map(|property| {
            PageProperty::new(
                property.key().to_owned(),
                property.normalized_key().to_owned(),
                property.value().to_owned(),
            )
        })
        .collect()
}

/// Property key that carries a block's folded state, as
/// [`Graph::set_block_collapsed`](super::Graph::set_block_collapsed) writes it.
pub(crate) const COLLAPSED_PROPERTY: &str = "collapsed";

/// The one value that property holds for a folded block.
pub(crate) const COLLAPSED_TRUE: &str = "true";

/// The one value that property holds for an unfolded block.
///
/// It is written, never removed: unfolding is a state the file states, so a caller that unfolds a
/// block it had folded gets `collapsed:: false` rather than a block with no property at all.
pub(crate) const COLLAPSED_FALSE: &str = "false";

/// The value [`COLLAPSED_PROPERTY`] holds for `collapsed`.
///
/// This is the only function that turns the flag into a value, so the standalone write
/// ([`Graph::set_block_collapsed`](super::Graph::set_block_collapsed)) and a committed
/// [`PageOperation::SetCollapsed`](crate::domain::PageOperation::SetCollapsed) cannot grow two
/// spellings of one state, and [`collapsed`] below reads the same literals it writes.
pub(crate) fn collapsed_value(collapsed: bool) -> &'static str {
    if collapsed {
        COLLAPSED_TRUE
    } else {
        COLLAPSED_FALSE
    }
}

/// Whether a block's facts state that it is folded.
///
/// This is the inverse of the write side and of nothing else. `Graph::set_block_collapsed`
/// (`engine/write.rs`) upserts the property `collapsed` with [`collapsed_value`], so the only
/// spelling that means folded is [`COLLAPSED_TRUE`], compared as
/// [`Property::value`](crate::document::Property::value) reports it — already trimmed, and
/// deliberately still case-sensitive, because accepting `True` would be a second rule and Logseq
/// writes the lowercase value. A block with no such property, with `false`, or with anything else is
/// unfolded.
///
/// The index stores no collapsed column, so there is no projection rule to align with; what this
/// must stay aligned with is the write, and the tests pair the two directly.
///
/// The lookup takes the last line under the key, and matches the key in its normalized form, which
/// is the same resolution the projection applies to `id::` below.
fn collapsed(facts: &BlockFacts) -> bool {
    facts
        .property(COLLAPSED_PROPERTY)
        .is_some_and(|property| property.value() == COLLAPSED_TRUE)
}
