//! Views over the content facts the graph states: page properties, backlinks, and tags.
//!
//! These are page-level views only. The index also holds per-block facts (`block_properties`,
//! `block_refs`, and the tags of one block). [`BlockProperties`] is the one block-level fact a
//! read exposes: it addresses the block by a public target (the identity decision of ADR-0005
//! stays with the engine layer) and answers with the lines the block states. No view here answers
//! with the other per-block facts yet.
//!
//! Every type is a value at one graph revision, like the rest of the domain: no IO, no SQL, and no
//! storage handle.

/// The property lines one block states, in written order.
///
/// Each element is the key and the value as written, like
/// [`PageProperty::key`](super::PageProperty::key) and
/// [`PageProperty::value`](super::PageProperty::value) for a page's lines. A repeated key appears
/// as many times as it was written; deciding that the last line under a key wins is the caller's
/// rule, not this type's.
pub type BlockProperties = Vec<(String, String)>;

/// The blocks at the sibling positions immediately before and after one block.
///
/// `None` on one side means the page's edge: there is no earlier or later sibling. Siblings are
/// the blocks that share a parent, and two top-level blocks are siblings of each other, so the
/// rule is about the position, not the nesting.
pub type BlockSiblings = (Option<super::BlockView>, Option<super::BlockView>);

/// One `key:: value` line of a page, in the spelling the text used.
///
/// A page's property list keeps every line, so a repeated key appears as many times as it was
/// written. Deciding that the last line under a key wins is the caller's rule, not this type's:
/// [`normalized_key`](Self::normalized_key) is what both lines share, so a caller that wants the
/// effective value can pick the last one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageProperty {
    key: String,
    normalized_key: String,
    value: String,
}

impl PageProperty {
    pub(crate) fn new(key: String, normalized_key: String, value: String) -> Self {
        Self {
            key,
            normalized_key,
            value,
        }
    }

    /// Key as written, without surrounding whitespace.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Key used for matching: lowercased with `_` replaced by `-`, the way Logseq names properties.
    ///
    /// This is the property rule, not the page-name rule
    /// ([`page_name_key`](super::page_name_key)): the two are deliberately different.
    pub fn normalized_key(&self) -> &str {
        &self.normalized_key
    }

    /// Value as written, without surrounding whitespace and with no newline in it.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// One page that references the queried page through `[[...]]`, with the referencing block that
/// stands for it.
///
/// The name is the spelling the referencing page was projected with, never the spelling the query
/// used, so it is the page name a caller can pass back to another page read.
///
/// A page can reference one target from several blocks, and the read collapses those into one
/// element, because the question is which pages point here. The block this element reports is the
/// page's **earliest** referencing block in preorder order: a deterministic representative, so the
/// position and the text belong to one block of one revision rather than to an arbitrary one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    page: String,
    preorder: u32,
    text: String,
    pre_block: bool,
}

impl Backlink {
    pub(crate) fn new(page: String, preorder: Option<u32>, text: String) -> Self {
        Self {
            page,
            preorder: preorder.unwrap_or_default(),
            text,
            pre_block: preorder.is_none(),
        }
    }

    /// Name of the page that contains the reference.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Depth-first position of the referencing block among its own page's blocks, starting at zero.
    ///
    /// This is the same position [`BlockNode::preorder`](super::BlockNode::preorder) reports, so a
    /// caller can locate the block in a page-tree read at the same revision. Like every preorder it
    /// is a position, not a block handle.
    pub fn preorder(&self) -> u32 {
        self.preorder
    }

    /// Whether the reference is in page properties, in which case there is no content-block
    /// position and `preorder()` is zero for compatibility.
    pub fn pre_block(&self) -> bool {
        self.pre_block
    }

    /// The referencing block's own text, as the projection reconstructed it.
    ///
    /// This is the block that holds the `[[...]]`, not the page text and not the target's text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// One tag of the graph and how many distinct pages use it.
///
/// The count is per page, not per block: a page that tags three different blocks with `#tag` is one
/// page using that tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagUsage {
    name: String,
    page_count: u32,
}

impl TagUsage {
    pub(crate) fn new(name: String, page_count: u32) -> Self {
        Self { name, page_count }
    }

    /// One stored spelling of the tag.
    ///
    /// Tags are identified by [`page_name_key`](super::page_name_key), so `#Tag` and `#tag` are one
    /// tag under two spellings. This is the byte-wise smallest spelling the graph stores for that
    /// key, which makes it deterministic across rebuilds; it is a label, not a second identity.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of distinct pages that use the tag.
    pub fn page_count(&self) -> u32 {
        self.page_count
    }
}
