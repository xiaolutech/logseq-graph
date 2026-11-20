//! Read queries over the derived rows.
//!
//! Reads rebuild a page tree from the stored preorder position and depth in one pass over rows
//! ordered by preorder. No source line is read or reconstructed here, and the rebuild uses an
//! explicit stack, so tree depth cannot overflow the thread stack.
//!
//! A read that needs more than one statement runs them inside one read transaction, so a concurrent
//! writer cannot land between them and hand back parts of two revisions (a page row with the block
//! rows of a later, or earlier, write). Reads take `&Connection`, so the transaction is opened with
//! [`Connection::unchecked_transaction`], or joins the caller's existing snapshot. A deferred read
//! transaction takes no write lock until a statement writes and never writes one here.
//!
//! The fact reads here are page-level: a page's properties, the pages that reference a page with the
//! block that represents each of them, the graph's tags, and the pages that use one tag. The
//! listings are page-level too: the page list with its block counts, and one page's blocks in
//! preorder with their text. Block-level reads address one located block by its `(page, preorder)`
//! pair: [`block_by_durable_id`] locates the block that states an `id::` UUID, and the property and
//! sibling reads below start from a view the engine has already located. Resolving a public target
//! to that location is the identity decision of ADR-0005 and stays with the engine layer, so every
//! block read here answers about a block something has already vouched for.
//!
//! Content search is the one read that compares text instead of a stored key. It scans the blocks'
//! own text for a literal substring and returns a page name, a preorder position, and a snippet; it
//! does not expose a block's properties, refs, or tags, and the position it returns is not identity.
//!
//! Every other read matches names by the keys the projection stored, never by comparing text: page
//! names and tags by [`page_name_key`], so `[[ä]]` finds the page `Ä`, while a property key keeps the
//! rule it was stored under. Both sides of every one of those comparisons are the same column kind,
//! which is what keeps each lookup an index seek instead of a scan.

#[cfg(doc)]
use crate::domain::page_name_key;
#[cfg(doc)]
use rusqlite::Connection;

mod block;
mod candidates;
pub(super) mod exec;
mod facts;
mod namespace;
mod page;
mod search;
#[cfg(test)]
mod test_support;

pub(super) use block::{
    block_by_durable_id, block_by_ephemeral_id, block_by_page_path, block_by_page_preorder,
    block_properties, sibling_views,
};
pub(super) use candidates::page_candidates;
pub(super) use facts::{backlinks, page_properties, pages_with_tag, tags};
pub(super) use namespace::{pages_from_namespace, pages_tree_from_namespace};
pub(super) use page::{page_block_views, page_file, page_metadata, page_names, page_tree, pages};
pub(super) use search::search;

// The selector one block row carries, from the two identity columns it stores.
//
// One decode shared by every reader of a block row: the table's CHECK makes exactly one of the two
// identity columns non-NULL, so a row with both or with neither is derived state this build cannot
// read back, and reporting it is what keeps a projection defect from reaching a caller as a block
// address that might name the wrong block.
use exec::selector as row_selector;
