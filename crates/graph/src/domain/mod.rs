//! Public data types returned by graph queries, and the identity rules over them.
//!
//! The domain layer holds plain values only: no filesystem access, no SQL, and no storage handles.
//! Every type here is a snapshot of derived state at one graph revision. The things here that
//! are not values are rules: [`page_name_key`], the rule every layer uses to decide whether two
//! spellings of a name denote one page, [`search_key`], the rule that decides whether block text
//! contains a searched substring, and the block identity types in `block`, which fix what
//! counts as one block. Rules belong to the domain, and no layer may hold a second version of
//! any of them.
//!
//! The third rule is the query grammar: [`QueryExpr`] is what a simple query means and [`QueryError`]
//! is the only way its text can fail, so the executor reads a typed tree and never parses text.

mod batch;
mod block;
mod block_view;
mod facts;
mod favorite;
mod graph;
mod journal_date;
mod kind;
mod move_intent;
mod name;
mod page;
mod page_candidate;
mod query;
mod read;
mod search;
mod snapshot;
mod task_marker;

pub use batch::{BatchMoveTarget, PageCommit, PageCommitResult, PageOperation};
pub use block::{
    BlockId, BlockIdError, BlockIdentityKind, BlockMarker, BlockSelector, EphemeralId,
    EphemeralIdError,
};
pub use block_view::BlockView;
pub use facts::{Backlink, BlockProperties, BlockSiblings, PageProperty, TagUsage};
pub use favorite::Favorite;
pub use graph::GraphMetadata;
pub use journal_date::JournalDate;
pub use kind::PageKind;
pub use move_intent::{MoveAnchorKind, MoveBlockAddress, MoveIntent};
pub use name::{list_value_items, list_value_matches, page_name_key};
pub use page::{BlockNode, PageMetadata, PageSummary, PageTree};
pub use page_candidate::{PageCandidate, PageMatch};
pub use query::{
    DateOperand, QueryBlock, QueryContext, QueryError, QueryExpr, QueryOutcome, QueryPage,
    QueryResult,
};
pub use read::{Read, ReadMeta};
pub use search::{search_key, search_key_char, SearchHit};
pub use snapshot::{
    BlockRefResolution, PageSnapshot, PageVersion, PageVersions, RootedSnapshot, SnapshotBlock,
};
pub use task_marker::{Priority, TaskMarker};
