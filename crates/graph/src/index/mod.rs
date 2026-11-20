//! Derived read model backed by SQLite.
//!
//! The index stores derived state projected from a parsed
//! [`PageDocument`](crate::document::PageDocument): page identity, block hierarchy, sibling and
//! preorder positions, block markers, each block's own text as `document::facts` reconstructs it,
//! the `id::` UUID a block states when that value is a UUID, and the content facts
//! [`PageFacts`](crate::document::PageFacts) extracts from the same parse, meaning page and block
//! properties, page refs, block refs, and tags. It never stores raw source
//! lines, source maps, or line numbers, because files stay authoritative and the derived state must
//! be reconstructible by parsing them again (ADR-0001, ADR-0002, ADR-0007). Block text is derived
//! content, not a raw line: the marker and the block's alignment indentation are removed and
//! continuation lines are joined before it is stored.
//!
//! This module is the only place in the crate that knows SQL, and there is no storage-backend
//! abstraction. A page is replaced as a whole: its previous rows are deleted by the page cascade
//! and the new complete page, facts included, is inserted in one transaction that advances the
//! graph revision exactly once (ADR-0004). Scans can batch whole-page replacements in one transaction
//! while retaining one revision per page. Block-level upserts do not exist.
//!
//! Projection and page-tree reads walk blocks with explicit stacks, so nesting depth is bounded by
//! [`MAX_BLOCK_DEPTH`] rather than by the thread stack. That limit also bounds the recursive drop
//! of a returned [`BlockNode`](crate::domain::BlockNode) tree: no deeper tree can be built.
//!
//! Facts are stored, not interpreted. A property value is the text after `::`, a page ref is the
//! name inside `[[...]]`, a block ref is the UUID inside `((...))`, and a tag is the name after `#`.
//! Logseq rules that read meaning into those strings are deliberately absent: a `tags::` value whose
//! text contains links does not create refs, a quoted property value does not suppress the refs its
//! text would otherwise produce, and no value is typed as a date, number, or list. Those rules depend
//! on `logseq/config.edn` and on Logseq compatibility, so they belong to a layer that still sees the
//! raw row. `document::facts` calls that layer "the projection layer" because it decides what a fact
//! means; writing the row is not deciding.
//!
//! Reads expose the page-level facts and one bounded content search: a page's properties, the pages
//! that reference a page, the graph's tags with their page counts, the pages that use one tag, and
//! the blocks whose stored text contains a substring. The page-fact reads answer with `None`, an
//! empty list, or a list ordered the way the query documents, always matched by the stored keys
//! rather than by comparing names as text. Search is the deliberate exception: it compares block
//! text, not names, and returns a position and a snippet rather than a fact. A block's own facts
//! (`block_properties`, `block_refs`, and the tags of one block) still have no public read here:
//! naming one block publicly needs ADR-0005's identity decision, so those rows are written,
//! cascaded, and tested through SQL but never addressed from outside this module. One internal
//! block reads exist beside them: `Index::block_by_durable_id` locates the block that states an
//! `id::` UUID, and `Index::block_by_ephemeral_id` locates the block one minted id names. Both
//! answer a location and leave the public-target decision to the engine layer; two blocks stating
//! one durable id is not a projection failure, so that lookup reports the ambiguity instead of
//! answering one of them, while an ephemeral id is unique by construction (ADR-0012).
//!
//! Scope of this slice: in-memory and on-disk databases, page projection including properties,
//! refs, tags, block text, a block's `id::` UUID, and a minted ephemeral id for a block that states
//! none, whole-page replacement with a recorded file baseline, page-metadata, page-tree,
//! page-property, backlink, tag, and bounded content-search reads, two internal block lookups (one
//! durable, one ephemeral), and the removal of rows a scan no longer finds a for. Public block-level reads, interpreting property values, and file watching are not
//! implemented here. Recovery state lives in the graph journal, not this disposable index.
//! The legacy intent reader exists only to export old rows before a schema rebuild.
//!
//! The index also imports two plain value types to describe a file baseline and a page row:
//! [`PageKind`](crate::domain::PageKind) from `domain` and [`FileMetadata`](crate::source::FileMetadata) from `source`. Neither
//! carries IO, so this stays a data dependency: `source` remains the only layer that reads files
//! and `index` remains the only layer that knows SQL.

mod baseline;
mod cjk_search;
mod error;
mod identity;
mod located;
mod open;
mod projection;
mod query;
use query::exec as query_exec;
mod read;
mod read_transaction;
use read_transaction::ReadTransaction;
mod scan;
mod schema;
mod storage;
#[cfg(test)]
mod test_support;
mod write;

pub use crate::domain::{MoveAnchorKind, MoveIntent};
pub use baseline::FileBaseline;
pub use error::IndexError;
pub(super) use error::{decode_marker, decode_page_kind};
pub(crate) use located::{DurableLookup, LocatedBlock, LocatedEphemeralBlock, LocatedPageBlock};
pub(crate) use projection::ProjectedPage as PreparedPage;
pub(crate) use query_exec::QueryExecError;
pub(crate) use scan::ScanPage;
pub(crate) use storage::Shadow;
pub use storage::StorageUsage;
pub(super) use write::advance_revision;

use rusqlite::{Connection, OptionalExtension};

/// Deepest block nesting the index accepts, as a `depth` value.
///
/// Depth counts from zero for top-level blocks, so this allows 256 nested levels. The limit sits
/// far above any real outliner page and low enough that walking or dropping a block tree cannot
/// exhaust the thread stack.
pub const MAX_BLOCK_DEPTH: u32 = 255;

/// Derived read model for one graph instance.
#[derive(Debug)]
pub struct Index {
    conn: Connection,
    location: Option<storage::Location>,
    read_only: bool,
    /// One-shot answer-read failure armed by an engine test; absent from production builds.
    #[cfg(test)]
    fail_next_answer: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// One-shot graph-metadata failure armed by an engine test; absent from production builds.
    #[cfg(test)]
    fail_next_metadata: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Row key of the page `name` names, matched by name key rather than by spelling.
///
/// A page's identity is [`page_name_key`](crate::domain::page_name_key), so `Ä` and `ä` resolve to one row here. The row keeps the
/// spelling it was projected with, which is what a read hands back.
fn find_page_id(conn: &Connection, name_key: &str) -> Result<Option<i64>, IndexError> {
    let id = conn
        .query_row(
            "SELECT id FROM pages WHERE name_key = ?1",
            [name_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(id)
}
