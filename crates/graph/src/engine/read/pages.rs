use crate::domain::{
    Backlink, BlockView, Favorite, GraphMetadata, PageMetadata, PageProperty, PageSummary,
    PageTree, Read, TagUsage,
};
use crate::engine::{EngineError, Freshness, Graph, Reads};
use crate::index::{Index, StorageUsage};
use std::path::Path;

impl Graph {
    /// Physical SQLite file selected by the latest published index generation.
    ///
    /// This does not scan the graph. Direct SQLite inspection and cache-size diagnostics use this
    /// path, not the logical index path supplied to OpenOptions.
    pub fn index_storage_path(&mut self) -> Result<Option<&Path>, EngineError> {
        self.refresh_index()?;
        Ok(self.index.storage_path())
    }

    /// Physical disk usage of the index: the live generation and any other generation files.
    ///
    /// Read from the cache directory as it is now, so it neither scans the graph nor waits for this
    /// handle to adopt a newer publication. `None` for an in-memory index.
    pub fn index_storage_usage(&self) -> Result<Option<StorageUsage>, EngineError> {
        Ok(self.index.storage_usage()?)
    }

    /// Identity of the derived instance and the last committed revision.
    ///
    /// Carries the freshness metadata every read result carries (ADR-0006).
    pub fn metadata(&mut self) -> Result<Read<GraphMetadata>, EngineError> {
        self.read(|index| index.metadata())
    }

    /// The page names `logseq/config.edn` lists under `:favorites`, in written order, each with
    /// whether a page of that name exists.
    ///
    /// The list is configuration, not derived state, so it is answered as written: a name with no
    /// page is a real state of the configuration, and dropping it would silently rewrite the list
    /// to match the current graph. [`Favorite::exists`] is the fact beside it, read from the same
    /// index revision the list is answered under, so a pinned page that was deleted, renamed, or
    /// never created is visible as such instead of vanishing.
    ///
    /// The names come from the source and never from a table: a configuration change edits this
    /// list without any page file changing, and a projected copy would answer the list of whichever
    /// scan last ran rather than the configuration the read observed.
    pub fn favorites(&mut self) -> Result<Read<Vec<Favorite>>, EngineError> {
        self.favorites_with(self.freshness)
    }

    /// [`Graph::favorites`] under `freshness`.
    fn favorites_with(&mut self, freshness: Freshness) -> Result<Read<Vec<Favorite>>, EngineError> {
        self.read_answer(freshness, |index, source| {
            let names = source.favorites();
            let mut favorites = Vec::with_capacity(names.len());
            for name in names {
                let exists = index.page_metadata(name)?.is_some();
                favorites.push(Favorite::new(name.clone(), exists));
            }
            Ok(favorites)
        })
    }

    /// Every page with its block count, ordered by folded page name.
    ///
    /// Both kinds come back in one list and each summary states which it is, so a caller that wants
    /// one kind filters on [`PageSummary::kind`] instead of asking per page. A caller that needs one
    /// page's blocks asks for them with [`Graph::page_blocks`].
    pub fn pages(&mut self) -> Result<Read<Vec<PageSummary>>, EngineError> {
        self.read(Index::pages)
    }

    /// Every block of one page in preorder order, with its text, or `None` when the page has no
    /// derived rows.
    ///
    /// This is the read a caller prints a page with. [`Graph::page_tree`] answers structure only, by
    /// design: attaching every block's text to the tree would make one read cost the page's full
    /// content. A caller that wants the text of a whole page therefore takes it from here in one
    /// query instead of one [`Graph::block`] read per block.
    ///
    /// Each view's [`selector`](BlockView::selector) is the address of the block in this revision,
    /// not the selector that asked for it, because no selector asked: a block that states an `id::`
    /// answers a durable selector, and a block without one answers the id the projection minted for
    /// it (ADR-0012). That is what lets a caller read a page once and then edit any block of it
    /// without a second read in between.
    pub fn page_blocks(&mut self, page: &str) -> Result<Read<Option<Vec<BlockView>>>, EngineError> {
        self.read(|index| index.page_blocks(page))
    }

    /// Metadata of one page, or `None` when the page has no derived rows.
    ///
    /// The page is matched by `page_name_key`, so `ä` and `Ä` ask for the same page.
    pub fn page_metadata(&mut self, name: &str) -> Result<Read<Option<PageMetadata>>, EngineError> {
        self.read(|index| index.page_metadata(name))
    }

    /// One page with its complete block tree, or `None` when the page has no derived rows.
    ///
    /// The page is matched by `page_name_key`, so `ä` and `Ä` ask for the same page; the tree's
    /// [`PageMetadata::name`] keeps the spelling the page was stored with.
    ///
    /// A node carries structure only: its `preorder`, its `sibling_index`, its depth, and its
    /// children. It deliberately carries no identity, so this read stays a structural one at a cost
    /// per node rather than per page. A caller that wants a block's text or its address reads
    /// [`Graph::page_blocks`], which answers every block of the page with the [`BlockView`] that
    /// names it; a caller that wants one block by target asks [`Graph::block`].
    pub fn page_tree(&mut self, name: &str) -> Result<Read<Option<PageTree>>, EngineError> {
        self.read(|index| index.page_tree(name))
    }

    /// Properties of one page in written order, or `None` when the page has no derived rows.
    ///
    /// These are the property lines of the page **preamble**, the text before the first block, which
    /// is where Logseq writes page properties (`key:: value` on its own line). Logseq also accepts a
    /// page property on the page's **first block** (`- key:: value`); that form is projected as a
    /// block property and does not appear here, so a page that only states its properties that way
    /// answers `Some` with an empty list. Merging the two is undecided: no fixture or spec in this
    /// repository says how Logseq orders a preamble property and a first-block property of the same
    /// key, so the compatibility slice decides it first and this read reports only what the
    /// projection stores as a page property.
    ///
    /// Every property line is one element, so a repeated key appears as often as it was written and
    /// "the last one wins" stays the caller's decision. A page that exists without property lines
    /// answers `Some` with an empty list, which is a different answer from a page that does not
    /// exist. The page is matched by `page_name_key`, like every other page read.
    pub fn page_properties(
        &mut self,
        name: &str,
    ) -> Result<Read<Option<Vec<PageProperty>>>, EngineError> {
        self.read(|index| index.page_properties(name))
    }

    /// Pages that reference `name` through `[[...]]`, each page once, ordered by folded page name.
    ///
    /// Names are compared by `page_name_key`, so `[[ä]]` is a backlink of the page `Ä` and
    /// `backlinks("Ä")` and `backlinks("ä")` answer the same. Several references from one page
    /// collapse into one element, because the question is which pages point here. The target need
    /// not exist: `[[new page]]` names a page that may have no file, and the pages that point at it
    /// still answer. A page that references itself is one of its own backlinks: the question is which
    /// pages point here, and the page that was asked about is not special-cased. Each
    /// [`Backlink::page`] is the referencing page's own stored spelling, never the spelling this call
    /// was made with.
    pub fn backlinks(&mut self, name: &str) -> Result<Read<Vec<Backlink>>, EngineError> {
        self.read(|index| index.backlinks(name))
    }

    /// Every tag of the graph with the number of distinct pages that use it, ordered by folded tag
    /// name.
    ///
    /// The count is per page, so tagging three blocks of one page with `#tag` counts once, and the
    /// tag is matched by `page_name_key`, so `#Tag` and `#tag` are one tag.
    pub fn tags(&mut self) -> Result<Read<Vec<TagUsage>>, EngineError> {
        self.read(Index::tags)
    }

    /// Names of the pages that use `tag`, each page once, ordered by folded page name.
    ///
    /// Matched by `page_name_key`, so any spelling of the tag names the same pages. Each returned
    /// name is the page's own stored spelling.
    pub fn pages_with_tag(&mut self, tag: &str) -> Result<Read<Vec<String>>, EngineError> {
        self.read(|index| index.pages_with_tag(tag))
    }

    /// Names of the pages that are direct children of one namespace, ordered by folded page name.
    ///
    /// A namespace is the part of a page name before a `/`: the children of `a` are the pages whose
    /// names begin with `a/` and hold no further slash, so `a/b` answers but `a/b/c` and `ab/c` do
    /// not. The namespace is matched by [`page_name_key`](crate::domain::page_name_key), the rule
    /// every page identity uses, so `A` and `ä` fold exactly as a `[[ref]]` target folds, and each
    /// returned name is the page's stored spelling.
    ///
    /// A namespace no page extends answers with an empty list, which is also what an empty namespace
    /// gives: no page name starts with a slash. An empty answer says "nothing under this name", not
    /// "the namespace is wrong".
    pub fn pages_from_namespace(
        &mut self,
        namespace: &str,
    ) -> Result<Read<Vec<String>>, EngineError> {
        self.read(|index| index.pages_from_namespace(namespace))
    }

    /// Names of every page under one namespace, the whole subtree, ordered by folded page name.
    ///
    /// Unlike [`Graph::pages_from_namespace`] this reports every depth: `a/b` and `a/b/c` both
    /// answer for the namespace `a`, still ordered by folded name so the subtree reads in page-list
    /// order. The matching and the empty answer follow the same rule as the direct-children read.
    pub fn pages_tree_from_namespace(
        &mut self,
        namespace: &str,
    ) -> Result<Read<Vec<String>>, EngineError> {
        self.read(|index| index.pages_tree_from_namespace(namespace))
    }
}

impl Reads<'_> {
    /// Identity of the derived instance and the last committed revision.
    pub fn metadata(&mut self) -> Result<Read<GraphMetadata>, EngineError> {
        self.graph.read_with(self.freshness, Index::metadata)
    }

    /// The page names `:favorites` lists, with whether each one exists.
    pub fn favorites(&mut self) -> Result<Read<Vec<Favorite>>, EngineError> {
        self.graph.favorites_with(self.freshness)
    }

    /// Every page with its block count, ordered by folded page name.
    pub fn pages(&mut self) -> Result<Read<Vec<PageSummary>>, EngineError> {
        self.graph.read_with(self.freshness, Index::pages)
    }

    /// Every block of one page in preorder order with its text, or `None` when the page has no
    /// derived rows.
    ///
    /// Same answer and same identity rule as [`Graph::page_blocks`].
    pub fn page_blocks(&mut self, page: &str) -> Result<Read<Option<Vec<BlockView>>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.page_blocks(page))
    }

    /// Metadata of one page, or `None` when the page has no derived rows.
    pub fn page_metadata(&mut self, name: &str) -> Result<Read<Option<PageMetadata>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.page_metadata(name))
    }

    /// One page with its complete block tree, or `None` when the page has no derived rows.
    ///
    /// A node carries structure only: its `preorder`, its `sibling_index`, its depth, and its
    /// children. See [`Graph::page_tree`] for why no node states an identity, and for the two reads
    /// that do.
    pub fn page_tree(&mut self, name: &str) -> Result<Read<Option<PageTree>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.page_tree(name))
    }

    /// Properties of one page in written order, or `None` when the page has no derived rows.
    pub fn page_properties(
        &mut self,
        name: &str,
    ) -> Result<Read<Option<Vec<PageProperty>>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.page_properties(name))
    }

    /// Pages that reference `name`, each page once, ordered by folded page name.
    pub fn backlinks(&mut self, name: &str) -> Result<Read<Vec<Backlink>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.backlinks(name))
    }

    /// Every tag of the graph with the number of distinct pages that use it.
    pub fn tags(&mut self) -> Result<Read<Vec<TagUsage>>, EngineError> {
        self.graph.read_with(self.freshness, Index::tags)
    }

    /// Names of the pages that use `tag`, each page once, ordered by folded page name.
    pub fn pages_with_tag(&mut self, tag: &str) -> Result<Read<Vec<String>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.pages_with_tag(tag))
    }

    /// Names of the pages that are direct children of one namespace, ordered by folded page name.
    pub fn pages_from_namespace(
        &mut self,
        namespace: &str,
    ) -> Result<Read<Vec<String>>, EngineError> {
        self.graph.read_with(self.freshness, |index| {
            index.pages_from_namespace(namespace)
        })
    }

    /// Names of every page under one namespace, the whole subtree, ordered by folded page name.
    pub fn pages_tree_from_namespace(
        &mut self,
        namespace: &str,
    ) -> Result<Read<Vec<String>>, EngineError> {
        self.graph.read_with(self.freshness, |index| {
            index.pages_tree_from_namespace(namespace)
        })
    }
}
