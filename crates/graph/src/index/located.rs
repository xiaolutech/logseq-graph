use super::{query, Index, IndexError};
use crate::domain::{BlockId, BlockMarker, BlockSelector, EphemeralId};

/// The stored block one durable id resolved to, with where it sits.
///
/// This is the payload of [`DurableLookup::One`], and nothing more: the page the block lives
/// on (by name as written), its preorder position in that page, its stored parent link, its parent's
/// preorder position, depth, and own text, and the normalized UUID it states. Turning the location
/// into a public target is a decision of the engine layer, which is why no selector or handle is
/// built here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocatedBlock {
    /// Name of the page the block belongs to, as written in `pages.name`.
    pub(crate) page: String,
    /// Depth-first position of the block within its page, starting at zero.
    pub(crate) preorder: u32,
    /// Stored row key of the parent block, or `None` for a top-level block.
    ///
    /// This is the row's private integer key, not public identity; reads that rebuild the tree use
    /// `depth` instead. It is returned so a caller inside this crate can address the parent row
    /// without a second lookup.
    pub(crate) parent_id: Option<i64>,
    /// Preorder position of the parent block, or `None` for a top-level block.
    ///
    /// Unlike `parent_id` this is a position in the page, so it is what a view can report: a caller
    /// walking up from one block needs the parent's preorder, not its row key.
    pub(crate) parent_preorder: Option<u32>,
    /// Nesting depth, starting at zero for top-level blocks.
    pub(crate) depth: u32,
    /// The block's own content, as the projection reconstructed it.
    pub(crate) text: String,
    /// The structural marker that opens the block.
    pub(crate) marker: BlockMarker,
    /// The `id::` UUID the block states, normalized to lowercase.
    pub(crate) durable_id: BlockId,
}

/// What the index knows about one durable id.
///
/// This is the answer of [`Index::block_by_durable_id`], and it is three-valued on purpose: two
/// blocks stating one `id::` is a state the files can be in (hand editing, a copy-paste that keeps
/// the identity line, two notes merged), and the projection records it as it is rather than letting
/// one file fail to project. Nothing in the schema makes the column unique, because deciding that
/// the files are wrong is not this layer's business; saying that it cannot answer is.
///
/// A caller that gets [`DurableLookup::Several`] has to decide what to do about it. That decision
/// differs between the read path, which cannot look at the files and must refuse, and the write
/// path, which reads the file and lets the document layer judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableLookup {
    /// No block states this id.
    Absent,
    /// Exactly one block states it.
    One(LocatedBlock),
    /// More than one block states it. `pages` are the distinct pages, sorted, and deduplicated.
    ///
    /// The sort is part of this answer's contract, not an implementation detail: it is what makes an
    /// error message built from it stable, and what lets a test assert the pages instead of the order
    /// the rows happened to come back in. Sorting is plain string order on the stored page names, not
    /// [`page_name_key`]: these names are what a person reads and searches for, and folding them here
    /// would answer with a spelling no file has.
    ///
    /// `pages.len() == 1` is still ambiguous: two blocks on one page both state the id.
    Several {
        /// Distinct page names stating the id, sorted.
        pages: Vec<String>,
    },
}

impl DurableLookup {
    /// The block that states the id, or `None` when zero or several do.
    ///
    /// Only for callers that have already handled ambiguity and need the plain answer; it exists so
    /// that a place which cannot proceed on `Several` writes that decision down instead of falling
    /// into a catch-all arm.
    pub(crate) fn into_one(self) -> Option<LocatedBlock> {
        match self {
            Self::One(located) => Some(located),
            Self::Absent | Self::Several { .. } => None,
        }
    }
}

/// One stored block located by a position in a page, with the identity the row states.
///
/// This is the answer of [`Index::block_by_page_path`] and [`Index::block_by_page_preorder`]. It
/// differs from [`LocatedBlock`] in one field: a position can name any block, so the selector here is
/// whichever kind the row carries rather than always the durable one. The page name is the spelling
/// `pages.name` stores, never the spelling the read was asked with.
///
/// A position is not an identity (ADR-0005): it is a coordinate inside one observation, which is why
/// the row's own selector comes with it instead of being derived from the position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocatedPageBlock {
    /// Name of the page the block belongs to, as written in `pages.name`.
    pub(crate) page: String,
    /// Depth-first position of the block within its page, starting at zero.
    pub(crate) preorder: u32,
    /// Preorder position of the parent block, or `None` for a top-level block.
    pub(crate) parent_preorder: Option<u32>,
    /// Nesting depth, starting at zero for top-level blocks.
    pub(crate) depth: u32,
    /// The block's own content, as the projection reconstructed it.
    pub(crate) text: String,
    /// The structural marker that opens the block.
    pub(crate) marker: BlockMarker,
    /// The identity this block's row states.
    pub(crate) selector: BlockSelector,
}

/// One stored block located by its ephemeral id, with what a write needs to reach it.
///
/// This is the answer of [`Index::block_by_ephemeral_id`], and it carries three things no other
/// located block does. `child_path` is the position the document layer edits by, derived from the
/// row's own parent chain rather than carried by the id, because the id says nothing about where the
/// block is. `page_fingerprint` is the content fingerprint of the bytes these rows were projected
/// from: a write compares it with the file it just read and refuses to touch the bytes when they
/// disagree (ADR-0012). Both exist so the caller never has to derive a position from anything but
/// these rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocatedEphemeralBlock {
    /// Name of the page the block belongs to, as written in `pages.name`.
    pub(crate) page: String,
    /// Depth-first position of the block within its page, starting at zero.
    pub(crate) preorder: u32,
    /// Preorder position of the parent block, or `None` for a top-level block.
    pub(crate) parent_preorder: Option<u32>,
    /// Nesting depth, starting at zero for top-level blocks.
    pub(crate) depth: u32,
    /// The block's own content, as the projection reconstructed it.
    pub(crate) text: String,
    /// The structural marker that opens the block.
    pub(crate) marker: BlockMarker,
    /// Child-index path from the page root down to this block, one element per level.
    ///
    /// This is what a document edit resolves: the position among the page's top-level blocks, then
    /// the position of each deeper block among its siblings.
    pub(crate) child_path: Vec<u32>,
    /// Content fingerprint of the page's bytes as this projection read them.
    pub(crate) page_fingerprint: u64,
}

impl Index {
    /// What the stored rows say about `id`: nothing, one block, or more than one.
    ///
    /// This is an internal read for the durable half of the block-target decision (ADR-0005): it
    /// locates the block and nothing more. Returning a public selector or a revision-bound handle
    /// is the engine layer's call, made from the page revision this row was read in. The page name
    /// is the spelling `pages.name` stores. Two blocks may state the same `id::`; no uniqueness is
    /// enforced here, and the read reports that as [`DurableLookup::Several`] instead of answering
    /// one of them.
    pub(crate) fn block_by_durable_id(&self, id: &BlockId) -> Result<DurableLookup, IndexError> {
        query::block_by_durable_id(&self.conn, id)
    }

    /// The block one ephemeral id names, or `None` when no row holds it.
    ///
    /// This is the whole of ephemeral resolution (ADR-0012): one equality on the unique
    /// `blocks.ephemeral_id` index, and nothing else. There is no fallback and no search, because an
    /// id that no row holds is an id whose block was re-projected, whose index was rebuilt, or which
    /// was never minted — in every case it names nothing, and looking for a block that resembles it
    /// would be the aliasing this identity exists to prevent.
    ///
    /// The answer carries the coordinate a write edits by and the fingerprint of the bytes the rows
    /// describe, so the caller can refuse to change a file those rows no longer match.
    pub(crate) fn block_by_ephemeral_id(
        &self,
        id: &EphemeralId,
    ) -> Result<Option<LocatedEphemeralBlock>, IndexError> {
        query::block_by_ephemeral_id(&self.conn, id)
    }

    /// The block at `preorder` on the page called `page`, or `None` when the page has no such block.
    ///
    /// This is the read a write uses to answer with the row it just committed: an edit keeps the
    /// block's preorder, while the page's ephemeral ids are all replaced by the re-projection that
    /// follows it, so the position is the only address that still names the block (ADR-0012).
    pub(crate) fn block_by_page_preorder(
        &self,
        page: &str,
        preorder: u32,
    ) -> Result<Option<LocatedPageBlock>, IndexError> {
        query::block_by_page_preorder(&self.conn, page, preorder)
    }

    /// The block named by `path` on the page called `page`, or `None` when nothing answers.
    ///
    /// This is an internal read for the ephemeral half of the block-target decision (ADR-0005).
    /// `path[0]` is the preorder of a top-level block and each later element is the position of that
    /// level's block among its siblings, so the descent names one block per level. A path that does
    /// not resolve — the page is gone, a level holds nothing at that position, or the block is
    /// absent — answers `None`; it never falls back to a neighbouring block.
    pub(crate) fn block_by_page_path(
        &self,
        page: &str,
        path: &[u32],
    ) -> Result<Option<LocatedPageBlock>, IndexError> {
        query::block_by_page_path(&self.conn, page, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_support::*;

    #[test]
    fn a_stated_id_becomes_the_blocks_durable_id() {
        let mut index = Index::open_in_memory().expect("open index");
        project(
            &mut index,
            "page",
            "- a\n  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n- b\n",
        )
        .expect("replace page");

        // The stored spelling is the lowercase one `domain::BlockId` normalizes to.
        assert_eq!(
            durable_id_at(&index, "page", 0),
            Some("01234567-89ab-cdef-0123-456789abcdef".to_owned())
        );
        assert_eq!(durable_id_at(&index, "page", 1), None);

        let DurableLookup::One(located) = index
            .block_by_durable_id(
                &"01234567-89AB-CDEF-0123-456789ABCDEF"
                    .parse()
                    .expect("valid UUID"),
            )
            .expect("read by durable id")
        else {
            panic!("exactly one block states the id");
        };
        assert_eq!(located.page, "page");
        assert_eq!(located.preorder, 0);
        assert_eq!(located.parent_id, None);
        assert_eq!(located.depth, 0);
        // The block's own text is what the same parse reconstructs, property lines included.
        assert_eq!(located.text, "a\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF");
        assert_eq!(
            located.durable_id.as_str(),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
        assert_eq!(
            index
                .block_by_durable_id(
                    &"99999999-9999-9999-9999-999999999999"
                        .parse()
                        .expect("valid UUID"),
                )
                .expect("read by durable id"),
            DurableLookup::Absent,
            "an id no block states answers Absent"
        );
    }

    #[test]
    fn only_the_block_that_states_an_id_has_one() {
        let mut index = Index::open_in_memory().expect("open index");
        project(
            &mut index,
            "page",
            "- a\n- b\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n- c\n",
        )
        .expect("replace page");

        assert_eq!(durable_id_at(&index, "page", 0), None);
        assert!(durable_id_at(&index, "page", 1).is_some());
        assert_eq!(durable_id_at(&index, "page", 2), None);
        assert_eq!(
            blocks_with_durable_id(&index),
            1,
            "exactly one of the three rows may hold a durable id"
        );
    }

    #[test]
    fn an_id_value_that_is_not_a_uuid_leaves_the_projection_empty_not_failed() {
        // The property line is ordinary text: the fact rows keep it verbatim, Logseq accepts such a
        // page, and only the shape check of `BlockId::parse` decides it is not identity. Refusing
        // the page would let one bad line make a whole file unreadable, so the block simply has no
        // durable id.
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "page", "- a\n  id:: not-a-uuid\n- b\n").expect("the page projects");

        assert_eq!(durable_id_at(&index, "page", 0), None);
        let value: String = index
            .conn
            .query_row(
                "SELECT block_properties.value
                   FROM block_properties
                   JOIN blocks ON blocks.id = block_properties.block_id
                  WHERE blocks.preorder = 0",
                [],
                |row| row.get(0),
            )
            .expect("read the property row");
        assert_eq!(value, "not-a-uuid", "the fact row keeps the text verbatim");
    }

    #[test]
    fn an_uppercase_id_key_is_recognized_like_a_lowercase_one() {
        // Property keys are matched in normalized form, so `ID::` and `Id::` reach the same lookup
        // as `id::`; Logseq treats them as the same property.
        let mut index = Index::open_in_memory().expect("open index");
        project(
            &mut index,
            "page",
            "- a\n  ID:: 01234567-89ab-cdef-0123-456789abcdef\n",
        )
        .expect("replace page");
        assert_eq!(
            durable_id_at(&index, "page", 0),
            Some("01234567-89ab-cdef-0123-456789abcdef".to_owned())
        );
    }

    #[test]
    fn a_repeated_id_key_keeps_the_last_value() {
        // The durable id follows the same last-one-wins rule a later read of the property rows
        // applies; the first line survives only in the fact rows.
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "page", "- a\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  id:: ffffffff-ffff-ffff-ffff-ffffffffffff\n",)
            .expect("replace page");
        assert_eq!(
            durable_id_at(&index, "page", 0),
            Some("ffffffff-ffff-ffff-ffff-ffffffffffff".to_owned())
        );
    }

    #[test]
    fn replacing_a_page_removes_the_old_blocks_durable_ids() {
        // Whole-page replacement deletes the old rows by the page cascade; the durable ids go with
        // the rows they decorate, so a lookup after the replacement finds nothing.
        let mut index = Index::open_in_memory().expect("open index");
        project(
            &mut index,
            "page",
            "- a\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
        )
        .expect("first replacement");
        assert!(durable_id_at(&index, "page", 0).is_some());

        project(&mut index, "page", "- only\n").expect("second replacement");
        assert_eq!(count(&index, "blocks"), 1);
        assert_eq!(
            blocks_with_durable_id(&index),
            0,
            "no block may keep a durable id after the page is replaced"
        );
        assert_eq!(durable_id_at(&index, "page", 0), None);
        assert_eq!(
            index
                .block_by_durable_id(
                    &"01234567-89ab-cdef-0123-456789abcdef"
                        .parse()
                        .expect("valid UUID"),
                )
                .expect("read by durable id"),
            DurableLookup::Absent,
        );
    }

    #[test]
    fn two_blocks_may_state_the_same_id() {
        // Duplicate durable identity is not rejected here: no ADR makes a stated `id::` unique, and
        // nothing in the projection or the schema enforces it. A real graph reaches this state by hand
        // editing, a copy-paste that keeps the identity line, or two notes merged, so the projection
        // records it as it is instead of failing the whole page. What the rows can say is that the id
        // is stated twice on this one page; deciding what that means belongs to the reader.
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "page", "- a\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n- b\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",)
            .expect("replace page");

        assert_eq!(
            index
                .block_by_durable_id(
                    &"01234567-89ab-cdef-0123-456789abcdef"
                        .parse()
                        .expect("valid UUID"),
                )
                .expect("read by durable id"),
            DurableLookup::Several {
                pages: vec!["page".to_owned()],
            },
            "both rows are on one page, and the page is named once"
        );
    }

    #[test]
    fn an_id_no_block_states_answers_absent() {
        let mut index = Index::open_in_memory().expect("open index");
        project(&mut index, "page", "- a\n- b\n").expect("replace page");

        assert_eq!(
            index
                .block_by_durable_id(
                    &"01234567-89ab-cdef-0123-456789abcdef"
                        .parse()
                        .expect("valid UUID"),
                )
                .expect("read by durable id"),
            DurableLookup::Absent
        );
    }

    #[test]
    fn an_id_stated_on_two_pages_answers_both_pages_in_order() {
        // The order is part of the answer, not an accident of the seek: an error message built from
        // it has to read the same way on every build, and a test cannot assert a set it was handed in
        // whatever order the planner produced. Pages are projected in reverse so a pass-through of
        // the row order would fail this assertion.
        let mut index = Index::open_in_memory().expect("open index");
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        project(&mut index, "zeta", &format!("- z\n  id:: {id}\n")).expect("project zeta");
        project(&mut index, "alpha", &format!("- a\n  id:: {id}\n")).expect("project alpha");

        assert_eq!(
            index
                .block_by_durable_id(&id.parse().expect("valid UUID"))
                .expect("read by durable id"),
            DurableLookup::Several {
                pages: vec!["alpha".to_owned(), "zeta".to_owned()],
            }
        );
    }

    #[test]
    fn every_page_stating_an_id_is_named_and_not_just_the_first_two() {
        // The answer doubles as the error message a person reads to find the duplicate, so it has to
        // name every page. A lookup that stopped at two rows would keep the third page invisible
        // until the first two were fixed, which is the kind of half-answer this read exists to
        // replace.
        let mut index = Index::open_in_memory().expect("open index");
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        for page in ["gamma", "alpha", "beta"] {
            project(&mut index, page, &format!("- block\n  id:: {id}\n")).expect("project page");
        }

        assert_eq!(
            index
                .block_by_durable_id(&id.parse().expect("valid UUID"))
                .expect("read by durable id"),
            DurableLookup::Several {
                pages: vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
            }
        );
    }

    #[test]
    fn an_id_stated_twice_on_one_page_names_that_page_once() {
        let mut index = Index::open_in_memory().expect("open index");
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        project(
            &mut index,
            "page",
            &format!("- a\n  id:: {id}\n- b\n  id:: {id}\n"),
        )
        .expect("replace page");

        assert_eq!(
            index
                .block_by_durable_id(&id.parse().expect("valid UUID"))
                .expect("read by durable id"),
            DurableLookup::Several {
                pages: vec!["page".to_owned()],
            },
            "the page is deduplicated, so the count says pages and not rows"
        );
    }
}
