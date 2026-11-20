//! The write path: append one block to one page, insert one new block at a position, replace the
//! text of one block, delete one block and its subtree, or move one block — within its page, or
//! across pages under a scoped write intent — file first and index second (ADR-0004).
//!
//! This module orchestrates and nothing else (ADR-0003). It takes the cross-process lock from
//! `source`, asks `source` for the files and their text, asks `document` for the pure edit, writes
//! the bytes back through `source`, and hands the re-read page to `index`. It holds no SQL and no
//! Markdown algorithm.
//!
//! The writes differ only in how the target is located and what they answer: an append names a page
//! and answers its metadata, an insert names a position beside an existing block and answers the
//! block it minted, an update names a block by either identity and answers the block it rewrote, a
//! remove names a block by either identity and answers its page's metadata, and a move names the
//! block to move plus the block it should end up next to, and answers its page's metadata. An
//! ephemeral selector resolves to one index row whose coordinates are checked against the bytes
//! before they are used, which is what makes writing through one safe (ADR-0012). The eight steps, and
//! the reason each one is where it is, are the same; a cross-page move is the one shape that
//! replaces two files, and it adds the intent ADR-0004 deferred to the first multi-file write. The
//! intent records each endpoint by durable id or a child-index path in its pre bytes, never an
//! ephemeral UUID; recovery resolves a source path only after matching the source pre hash and
//! destination post hash, then checks the derived source post hash before writing. An
//! insert has no such shape to add: the new block lives where its anchor lives, so it is always one
//! file's replacement.
//!
//! The order below is the decision, so each step states what it protects. A write crosses two
//! systems that cannot share a transaction, and the ordering is what keeps the derived model from
//! ever describing content that does not exist.
//!
//! The configuration-generation gate may fully reproject before any target is resolved. The
//! no-scan descriptions below apply once source and index configurations match.
//!
//! With matching configurations, neither write scans the graph first. The bytes an
//! edit transforms are the bytes this write just read from the file it is about to replace, and the
//! result is a whole-page replacement; no index row contributes to them, so an index page that is
//! behind its file cannot make this write lose content. [`Graph::scan`] answers "which files
//! changed", which is a question a *read* asks: a caller that wants the whole graph refreshed before
//! it writes reads once with [`Freshness::VerifyContent`] and pays that price itself. Two
//! exceptions exist, and both are narrow. Resolving a page *name* to its file is one query against
//! the path the projection stored, not a scan, and only a stale or absent row pays for discovery.
//! [`Graph::update_block`] resolves its target through the index and therefore pays for a scan on
//! the rare miss — see its step 2.
//!
//! Recovery of an interrupted cross-page move lives here too, because it is the same decision read
//! backwards: a surviving [`MoveIntent`] is the one state a scan cannot interpret, so
//! [`Graph::open`] reconciles it before anything else, with the startup scan off or on.
//!
//! [`Freshness::VerifyContent`]: crate::engine::Freshness::VerifyContent

#[cfg(doc)]
use crate::engine::Graph;
#[cfg(doc)]
use crate::index::MoveIntent;

mod commit;
mod edit;
mod error;
mod identity;
mod insert;
mod moves;
mod page;
mod recovery;
mod remove;
mod target;

#[cfg(test)]
mod test_support;

pub use error::{WriteError, WriteState};
pub use identity::EnsuredBlockId;
pub use moves::MoveTarget;

/// A one-shot post-commit failure the engine tests can arm without touching production code.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestFault {
    /// Fail the re-observation that follows the replacement.
    ReReadAfterWrite,
    /// Fail the index replacement with a real trigger, not a synthesized error value.
    IndexAfterWrite,
    /// Fail the source replacement in a cross-page move after the destination changed.
    SourceAfterDestination,
}
