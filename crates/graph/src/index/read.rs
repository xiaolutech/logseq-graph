use std::path::{Path, PathBuf};

use super::{baseline, query, schema, FileBaseline, Index, IndexError};
use crate::domain::{
    Backlink, BlockSiblings, BlockView, GraphMetadata, PageCandidate, PageMetadata, PageProperty,
    PageSummary, PageTree, SearchHit, TagUsage,
};
use rusqlite::OptionalExtension;

impl Index {
    pub(crate) fn snapshot<T, E: From<IndexError>>(
        &self,
        read: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let transaction = self
            .conn
            .unchecked_transaction()
            .map_err(IndexError::from)?;
        let result = read(self);
        drop(transaction);
        result
    }

    /// Identity of this graph instance and its last committed revision.
    pub fn metadata(&self) -> Result<GraphMetadata, IndexError> {
        #[cfg(test)]
        if self
            .fail_next_metadata
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(IndexError::CorruptRow {
                column: "graph_metadata.revision",
                value: "read rejected by engine test".to_owned(),
            });
        }
        schema::read_metadata(&self.conn)
    }

    /// Baseline recorded for `relative_path`, or `None` when the file was never projected.
    pub fn file_baseline(&self, relative_path: &Path) -> Result<Option<FileBaseline>, IndexError> {
        baseline::read(&self.conn, relative_path)
    }

    /// Every recorded baseline path, ordered by path.
    ///
    /// The caller compares this with the files it discovered; a path that is no longer there belongs
    /// to a deleted or renamed file.
    pub fn file_baseline_paths(&self) -> Result<Vec<PathBuf>, IndexError> {
        baseline::paths(&self.conn)
    }

    /// Every stored page name, ordered by name.
    ///
    /// The caller compares this with the names it discovered; a name that is no longer produced by
    /// any file belongs to a deleted or renamed page.
    pub fn page_names(&self) -> Result<Vec<String>, IndexError> {
        query::page_names(&self.conn)
    }

    pub(crate) fn configuration_matches(&self, signature: &str) -> Result<bool, IndexError> {
        Ok(self.conn.query_row(
            "SELECT coalesce(config_signature = ?1, 0) FROM graph WHERE id = 1",
            [signature],
            |row| row.get(0),
        )?)
    }

    /// Name of the page projected from `relative_path`, or `None` when no page names this file.
    ///
    /// The inverse of [`Self::page_file`], for a caller that already knows which file changed and
    /// needs the page rows it backs — a per-path refresh must remove those rows by name, because
    /// removal is keyed by [`page_name_key`](crate::domain::page_name_key), not by path. It is one seek on the unique
    /// `pages.relative_path`, so a path can back at most one page and the answer cannot be
    /// ambiguous.
    pub fn page_name_for_path(&self, relative_path: &Path) -> Result<Option<String>, IndexError> {
        let name = self
            .conn
            .query_row(
                "SELECT name FROM pages WHERE relative_path = ?1",
                [relative_path
                    .to_str()
                    .ok_or_else(|| IndexError::NonUtf8Path {
                        path: relative_path.to_owned(),
                    })?],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(name)
    }

    /// Every page with its block count, ordered by folded page name.
    ///
    /// Both kinds are listed by one query and each row states which it is, so a caller filters
    /// instead of asking again.
    pub fn pages(&self) -> Result<Vec<PageSummary>, IndexError> {
        query::pages(&self.conn)
    }

    /// Every block of one page in preorder order with its text, or `None` when the page has no
    /// derived rows.
    ///
    /// The page is matched by [`page_name_key`](crate::domain::page_name_key), so `ä` finds the page stored as `Ä`, and every view
    /// carries the stored spelling. This is the read a page listing needs: [`Self::page_tree`]
    /// answers structure only and a search hit answers one block plus a snippet.
    pub fn page_blocks(&self, page: &str) -> Result<Option<Vec<BlockView>>, IndexError> {
        query::page_block_views(&self.conn, page)
    }

    /// Metadata of one page, or `None` when the page has no derived rows.
    ///
    /// The page is matched by [`page_name_key`](crate::domain::page_name_key), so `ä` finds the page stored as `Ä`. The returned
    /// name is the spelling `pages.name` holds, not the one the caller asked with.
    pub fn page_metadata(&self, name: &str) -> Result<Option<PageMetadata>, IndexError> {
        #[cfg(test)]
        if self
            .fail_next_answer
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(IndexError::CorruptRow {
                column: "pages.name",
                value: name.to_owned(),
            });
        }
        query::page_metadata(&self.conn, name)
    }

    /// Graph-relative path of the file the page `name` was projected from, or `None` when the page
    /// has no derived rows.
    ///
    /// This is the answer to "which file holds this page", and it is one indexed read of
    /// `pages.relative_path`, the column that pairs a page row with the baseline of its file.
    /// A caller that needs the whole [`PageFile`](crate::source::PageFile) can ask
    /// [`GraphSource::page_at`](crate::source::GraphSource::page_at) for exactly this path instead
    /// of listing the graph again.
    ///
    /// The path is the one a scan recorded, so it can be stale: a file renamed outside this process
    /// leaves the row pointing at the old name until something projects or removes the page. A
    /// caller that cannot use the file it finds at this path must fall back to discovery, which is
    /// the only authority on what the files are now.
    pub fn page_file(&self, name: &str) -> Result<Option<PathBuf>, IndexError> {
        query::page_file(&self.conn, name)
    }

    /// One page with its complete block tree, or `None` when the page has no derived rows.
    ///
    /// The page is matched by [`page_name_key`](crate::domain::page_name_key), so `ä` finds the page stored as `Ä`.
    pub fn page_tree(&self, name: &str) -> Result<Option<PageTree>, IndexError> {
        query::page_tree(&self.conn, name)
    }

    /// Properties of one page in written order, or `None` when the page has no derived rows.
    ///
    /// Only preamble property lines are returned. A page property written on the page's first block
    /// (`- key:: value`, which Logseq also accepts) is projected as a block property and is not part
    /// of this list; see [`Graph::page_properties`](crate::engine::Graph::page_properties) for why
    /// the two are not merged here.
    ///
    /// A page with no property lines answers `Some` with an empty list, which is a different answer
    /// from a page that has no rows at all. Repeated keys keep every line; picking the last one is
    /// the caller's rule. The page is matched by [`page_name_key`](crate::domain::page_name_key), like every other page read.
    pub fn page_properties(&self, name: &str) -> Result<Option<Vec<PageProperty>>, IndexError> {
        query::page_properties(&self.conn, name)
    }

    /// Pages that reference `name` through `[[...]]`, each page once, ordered by folded page name.
    ///
    /// The comparison is on [`page_name_key`](crate::domain::page_name_key), so `[[ä]]` references the page `Ä`. The target need
    /// not be a stored page: an unresolved reference still answers with the pages that point at it.
    pub fn backlinks(&self, name: &str) -> Result<Vec<Backlink>, IndexError> {
        query::backlinks(&self.conn, name)
    }

    /// Every tag with the number of distinct pages that use it, ordered by folded tag name.
    pub fn tags(&self) -> Result<Vec<TagUsage>, IndexError> {
        query::tags(&self.conn)
    }

    /// Names of the pages that use `tag`, each page once, ordered by folded page name.
    ///
    /// Matched by [`page_name_key`](crate::domain::page_name_key), so `#Tag` and `#tag` ask for the same pages.
    pub fn pages_with_tag(&self, tag: &str) -> Result<Vec<String>, IndexError> {
        query::pages_with_tag(&self.conn, tag)
    }

    /// Blocks whose text contains `query`, at most `limit` of them.
    ///
    /// The match is a case-insensitive substring of the stored block text, not a word or fuzzy
    /// match. See `query::search` for the exact folding rule and the snippet shape. A `limit` of
    /// zero answers with no hits, and there is no unbounded form: every call is bounded.
    ///
    /// The returned order is the order the scan produced, which is unspecified; callers must not
    /// use it as a rank. A hit names its page as written and the block's preorder position in the
    /// page, which is a position inside one revision and not a stable block handle (ADR-0005).
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, IndexError> {
        query::search(&self.conn, query, limit)
    }

    /// Pages a name lookup can jump to, at most `limit` of them.
    ///
    /// A page answers through its own name and through the values of its `alias::` property. The
    /// list is ordered: exact before prefix before infix, and a page-name hit before any alias
    /// hit, so the first entry is the default jump. A shared alias yields one entry per page
    /// rather than one merged entity, and a caller that needs a single page picks from the list.
    /// Both the page name and every alias value are compared under [`page_name_key`](crate::domain::page_name_key), the one
    /// page-name rule, so `ZMI` finds an `alias:: zmi` line.
    pub fn page_candidates(
        &self,
        text: &str,
        limit: u32,
    ) -> Result<Vec<PageCandidate>, IndexError> {
        query::page_candidates(&self.conn, text, limit)
    }

    /// Property lines of one block, in written order, or `None` when the page has no derived rows.
    ///
    /// The block is addressed by the `(page, preorder)` pair of a view the engine has already
    /// located by public identity (ADR-0005), so this layer never decides what a selector names.
    /// The page is matched by [`page_name_key`](crate::domain::page_name_key), so any spelling of the page's name answers for it.
    /// A block without property lines answers with an empty list, and a repeated key keeps every
    /// line; picking the last one is the caller's rule.
    pub(crate) fn block_properties(
        &self,
        page: &str,
        preorder: u32,
    ) -> Result<Option<Vec<(String, String)>>, IndexError> {
        query::block_properties(&self.conn, page, preorder)
    }

    /// The blocks on the previous and the next sibling position of one block, or `None` when the
    /// page has no derived rows.
    ///
    /// Siblings share a parent, and top-level blocks are siblings of each other, so a `None` here is
    /// the page's edge rather than a rule about nesting. The views carry the same identity rule as
    /// [`Self::page_blocks`]: a block that states an `id::` is `Durable`, one without is `Ephemeral`.
    pub(crate) fn sibling_views(
        &self,
        page: &str,
        preorder: u32,
    ) -> Result<Option<BlockSiblings>, IndexError> {
        query::sibling_views(&self.conn, page, preorder)
    }

    /// Names of the pages that are direct children of one namespace, ordered by folded page name.
    ///
    /// Namespaces are the `/` in page names, so `a/b` is a child of `a`. The namespace is matched
    /// through [`page_name_key`](crate::domain::page_name_key), and every returned name is the stored spelling. A namespace that
    /// no page extends answers with an empty list, which is also what an empty namespace gives: no
    /// page name starts with a slash.
    pub(crate) fn pages_from_namespace(&self, namespace: &str) -> Result<Vec<String>, IndexError> {
        query::pages_from_namespace(&self.conn, namespace)
    }

    /// Names of every page under one namespace, the whole subtree, ordered by folded page name.
    ///
    /// Unlike [`Self::pages_from_namespace`] this reports every depth: `a/b` and `a/b/c` both answer
    /// for the namespace `a`.
    pub(crate) fn pages_tree_from_namespace(
        &self,
        namespace: &str,
    ) -> Result<Vec<String>, IndexError> {
        query::pages_tree_from_namespace(&self.conn, namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::page_name_key;
    use crate::index::test_support::*;

    #[test]
    fn search_returns_page_preorder_and_a_bounded_verbatim_snippet() {
        let mut index = Index::open_in_memory().expect("open index");
        let long = format!("- {}NEEDLE{}\n", "a".repeat(200), "b".repeat(200));
        project(&mut index, "Long Page", &long).expect("replace page");

        let hits = index.search("needle", 5).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page(), "Long Page", "the page name as written");
        assert_eq!(hits[0].preorder(), 0);
        let snippet = hits[0].snippet();
        assert!(snippet.contains("NEEDLE"), "the match stays in: {snippet}");
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
        // 2 * 48 context characters, the six-character query, and at most two ellipses.
        assert!(
            snippet.chars().count() <= 2 * 48 + "needle".len() + 2,
            "the snippet must stay bounded, got {} characters",
            snippet.chars().count()
        );
        assert!(
            snippet.len() < long.len(),
            "a long block must not be returned whole"
        );
    }

    #[test]
    fn search_is_literal_bounded_and_replaced_with_the_page() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "a", "- needle one\n- needle two\n").expect("replace a");
        project(&mut index, "b", "- needle three\n").expect("replace b");

        assert_eq!(index.search("needle", 10).expect("search").len(), 3);
        assert_eq!(index.search("needle", 1).expect("search").len(), 1);
        assert_eq!(index.search("needle", 100).expect("search").len(), 3);
        assert!(
            index.search("needle", 0).expect("search").is_empty(),
            "a zero limit asks for nothing"
        );
        assert!(
            index.search("", 10).expect("search").is_empty(),
            "an empty query is not a wildcard"
        );
        // A substring, not a word: the match may start inside a word.
        assert_eq!(index.search("eedl", 10).expect("search").len(), 3);
        // ASCII case folding, which is all SQLite's `lower` does.
        assert_eq!(index.search("NEEDLE", 10).expect("search").len(), 3);

        // Replacing a page replaces its searchable text in the same transaction.
        project(&mut index, "a", "- changed\n").expect("replace a again");
        assert_eq!(index.search("needle", 10).expect("search").len(), 1);
        assert_eq!(index.search("changed", 10).expect("search").len(), 1);
    }

    #[test]
    fn search_folds_unicode_case_but_not_like_the_page_key() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "Ärger", "- ÄRGER\n- Straße\n").expect("replace page");

        // The page-name key folds Unicode, so the page is the same page under either spelling.
        assert!(index.page_metadata("ärger").expect("read page").is_some());
        // Text matching folds Unicode too, but through its own length-preserving rule.
        assert_eq!(index.search("ärger", 10).expect("search").len(), 1);
        assert_eq!(index.search("Ärger", 10).expect("search").len(), 1);
        assert_eq!(index.search("ÄRGER", 10).expect("search").len(), 1);
        assert_eq!(index.search("STRASSE", 10).expect("search").len(), 0);
        // Identity may fold `İ` to two characters and U+212A to ASCII `k`; a search match position
        // has to name a position in the stored text, so both stay as written here.
        project(&mut index, "corners", "- İstanbul \u{212a}\n").expect("replace page");
        assert_eq!(index.search("istanbul", 10).expect("search").len(), 0);
        assert_eq!(index.search("İSTANBUL", 10).expect("search").len(), 1);
        assert_eq!(index.search("k", 10).expect("search").len(), 0);
        assert_eq!(index.search("\u{212a}", 10).expect("search").len(), 1);
    }

    #[test]
    fn a_unicode_match_keeps_the_snippet_window_on_the_stored_text() {
        let mut index = Index::open_in_memory().expect("open index");
        let long = format!("- {}ÄRGER{}\n", "a".repeat(200), "b".repeat(200));
        project(&mut index, "Long Page", &long).expect("replace page");

        let hits = index.search("ärger", 5).expect("search");
        assert_eq!(hits.len(), 1);
        let snippet = hits[0].snippet();
        assert!(snippet.contains("ÄRGER"), "the match stays in: {snippet}");
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
        // The fold keeps the character count, so the SQL window still cuts the stored text.
        assert!(snippet.chars().count() <= 2 * 48 + "ärger".chars().count() + 2);
    }

    #[test]
    fn a_page_ref_is_found_under_any_spelling_of_its_name() {
        let mut index = Index::open_in_memory().expect("open index");
        project(
            &mut index,
            "refs",
            "- a [[Logseq]]\n- b [[logseq]]\n- c [[LOGSEQ]] [[logseq]]\n- d [[Ä]] [[ä]]\n",
        )
        .expect("replace page");

        // One row per (block, target key), and the first spelling in a block is the one kept: a
        // block that writes the same link twice, in any two spellings, states one fact. The last
        // block proves the key is Unicode-aware and not the ASCII fold SQLite offers.
        assert_eq!(
            fact_rows(&index, PAGE_REFS, "refs"),
            [
                "0|Logseq|logseq",
                "1|logseq|logseq",
                "2|LOGSEQ|logseq",
                "3|Ä|ä"
            ]
        );

        // The reverse lookup is what backlinks will run, and it is given a key, not a spelling.
        let found = |key: &str| -> i64 {
            index
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM page_refs WHERE target_key = ?1",
                    [key],
                    |row| row.get(0),
                )
                .expect("count backlinks")
        };
        assert_eq!(found(&page_name_key("loGsEq")), 3);
        assert_eq!(found(&page_name_key("Ä")), 1);
        // The key is not a superset of anything: a tag is not a page ref.
        assert_eq!(found(&page_name_key("absent")), 0);
    }

    #[test]
    fn a_backlink_from_another_page_is_found_through_the_page_key() {
        let mut index = Index::open_in_memory().expect("open index");
        // `Ä` references `ä`, which is itself under the page-name rule, and `Über` references the
        // same page. Three spellings, one page, two sources.
        project(&mut index, "Ä", "- 目标\n- 自引用 [[ä]]\n").expect("replace Ä");
        project(&mut index, "Über", "- 引用 [[ä]]\n").expect("replace Über");

        // The stored page key is the Unicode fold, which is what makes the join below possible: an
        // ASCII-only fold would store `Ä` here and never match the `ä` the ref was keyed with.
        assert_eq!(
            all_rows(
                &index,
                "SELECT name || '|' || name_key FROM pages ORDER BY name"
            ),
            ["Ä|ä", "Über|über"]
        );

        let sources = fact_rows(
            &index,
            "\
SELECT source.name || '|' || blocks.preorder
  FROM pages AS target
  JOIN page_refs ON page_refs.target_key = target.name_key
  JOIN blocks ON blocks.id = page_refs.block_id
  JOIN pages AS source ON source.id = blocks.page_id
 WHERE target.name_key = ?1
 ORDER BY source.name, blocks.preorder",
            &page_name_key("Ä"),
        );

        // Both spellings of the target resolve to the page `Ä`, and both referencing blocks are
        // found. A name column compared as written finds nothing here: `Ä` is not `ä`.
        assert_eq!(sources, ["Ä|1", "Über|0"]);
    }
}
